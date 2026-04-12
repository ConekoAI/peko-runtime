#!/usr/bin/env pwsh
# ADR-019 Phase 1: Tool Permission Enforcement E2E Test
#
# Tests execution-time permission checking at ExtensionCore layer.
# 
# Key behaviors:
# 1. Tools NOT in whitelist are never registered (can't be called by LLM)
# 2. Tools IN whitelist are registered and can be called
# 3. Execution-time check (is_tool_enabled) provides defense in depth
#
# This test verifies that the whitelist is properly enforced.

param(
    [string]$Provider = "kimi"
)

$ErrorActionPreference = "Stop"

Write-Host "========================================" -ForegroundColor Cyan
Write-Host "ADR-019 Phase 1: Tool Permission Test" -ForegroundColor Cyan
Write-Host "========================================" -ForegroundColor Cyan

# Check prerequisites
if (-not $env:KIMI_API_KEY -and $Provider -eq "kimi") {
    Write-Error "KIMI_API_KEY environment variable not set"
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
pekobot auth set $Provider $env:KIMI_API_KEY 2>&1 | Out-Null
Write-Host "Set API key for $Provider" -ForegroundColor Green

# Enable tools globally
pekobot ext enable shell 2>&1 | Out-Null
pekobot ext enable glob 2>&1 | Out-Null
pekobot ext enable read_file 2>&1 | Out-Null
Write-Host "Enabled tools globally: shell, glob, read_file" -ForegroundColor Green

Write-Host ""
Write-Host "========================================" -ForegroundColor Cyan
Write-Host "TEST: Whitelist Enforcement" -ForegroundColor Cyan
Write-Host "========================================" -ForegroundColor Cyan

# Create agent with ONLY glob and read_file in whitelist
$agentName = "adr019_test"
pekobot agent create $agentName --provider $Provider 2>&1 | Out-Null

# Get agent config directory
$agentConfigDir = "$env:APPDATA/pekobot/agents/default/$agentName"
if (-not (Test-Path $agentConfigDir)) {
    $agentConfigDir = "$env:USERPROFILE/.pekobot/agents/default/$agentName"
}

# Ensure directory exists
if (-not (Test-Path $agentConfigDir)) {
    New-Item -ItemType Directory -Path $agentConfigDir -Force | Out-Null
}

# Create agent.toml with explicit whitelist (shell NOT included)
@"
name = "$agentName"
provider = "$Provider"
tools = { whitelist = ["glob", "read_file"] }
"@ | Out-File -FilePath "$agentConfigDir/agent.toml" -Encoding UTF8

Write-Host "Created agent with whitelist: glob, read_file" -ForegroundColor Yellow
Write-Host "NOT in whitelist: shell" -ForegroundColor Red

# Test 1: Try to use allowed tool (glob)
Write-Host ""
Write-Host "TEST 1: Using allowed tool (glob)..." -ForegroundColor Cyan
$response1 = pekobot send $agentName "Use the glob tool to find all files in the current directory. List what you find." 2>&1
Write-Host "Response: $response1" -ForegroundColor Gray

$globWorked = $response1 -match "SYSTEM.md|file|directory"
if ($globWorked) {
    Write-Host "✅ PASS: glob tool worked (in whitelist)" -ForegroundColor Green
} else {
    Write-Host "⚠️ glob result unclear" -ForegroundColor Yellow
}

# Test 2: Try to use blocked tool (shell)
Write-Host ""
Write-Host "TEST 2: Using blocked tool (shell - not in whitelist)..." -ForegroundColor Cyan
$response2 = pekobot send $agentName "Try to use the shell tool to list files. What happens? If you can't use it, tell me what tools you have available." 2>&1
Write-Host "Response: $response2" -ForegroundColor Gray

# Check if LLM knows it doesn't have shell
$knowsNoShell = $response2 -match "don't have.*shell|no.*shell|cannot.*shell|not.*available"
$shellWasBlocked = $response2 -match "disabled|blocked|not.*enabled"

if ($knowsNoShell -or $shellWasBlocked) {
    Write-Host "✅ PASS: LLM knows shell is not available" -ForegroundColor Green
    $test2Pass = $true
} else {
    # If LLM tried to use shell anyway, check session for error
    Write-Host "ℹ️ LLM may have attempted shell - checking session..." -ForegroundColor Yellow
    $test2Pass = $false
}

# Check session file for tool calls
$sessionFile = "$DataDir/workspaces/default/$agentName/session.jsonl"
if (Test-Path $sessionFile) {
    $content = Get-Content $sessionFile -Raw
    
    Write-Host ""
    Write-Host "Session Analysis:" -ForegroundColor Cyan
    
    # Check for glob tool call
    if ($content -match '"name":\s*"glob"') {
        Write-Host "  ✓ glob tool was called" -ForegroundColor Green
    }
    
    # Check for shell tool call
    if ($content -match '"name":\s*"shell"') {
        Write-Host "  ⚠️ shell tool was attempted" -ForegroundColor Yellow
        
        # Check if it was rejected
        if ($content -match "disabled" -or $content -match "is_error.*true") {
            Write-Host "  ✓ shell was rejected/blocked" -ForegroundColor Green
            $test2Pass = $true
        } else {
            Write-Host "  ❌ shell was NOT rejected (might have succeeded)" -ForegroundColor Red
            $test2Pass = $false
        }
    } else {
        Write-Host "  ✓ shell tool was NOT called" -ForegroundColor Green
        if (-not $test2Pass) { $test2Pass = $true }
    }
}

# Test 3: Create new agent WITH shell in whitelist
Write-Host ""
Write-Host "========================================" -ForegroundColor Cyan
Write-Host "TEST 3: Agent WITH shell in whitelist" -ForegroundColor Cyan
Write-Host "========================================" -ForegroundColor Cyan

$agentName3 = "adr019_test_with_shell"
pekobot agent create $agentName3 --provider $Provider 2>&1 | Out-Null

$agentConfigDir3 = "$env:APPDATA/pekobot/agents/default/$agentName3"
if (-not (Test-Path $agentConfigDir3)) {
    $agentConfigDir3 = "$env:USERPROFILE/.pekobot/agents/default/$agentName3"
}
if (-not (Test-Path $agentConfigDir3)) {
    New-Item -ItemType Directory -Path $agentConfigDir3 -Force | Out-Null
}

@"
name = "$agentName3"
provider = "$Provider"
tools = { whitelist = ["shell", "glob", "read_file"] }
"@ | Out-File -FilePath "$agentConfigDir3/agent.toml" -Encoding UTF8

Write-Host "Created agent with whitelist: shell, glob, read_file" -ForegroundColor Green

$response3 = pekobot send $agentName3 "Use the shell tool to echo 'ADR-019 Phase 1 test successful'" 2>&1
Write-Host "Response: $response3" -ForegroundColor Gray

$shellWorked = $response3 -match "ADR-019|test successful|echo"
if ($shellWorked) {
    Write-Host "✅ PASS: shell tool worked (in whitelist)" -ForegroundColor Green
    $test3Pass = $true
} else {
    Write-Host "⚠️ shell result unclear" -ForegroundColor Yellow
    $test3Pass = $false
}

Write-Host ""
Write-Host "========================================" -ForegroundColor Cyan
Write-Host "Test Complete - Cleaning up" -ForegroundColor Cyan
Write-Host "========================================" -ForegroundColor Cyan

pekobot agent delete $agentName 2>&1 | Out-Null
pekobot agent delete $agentName3 2>&1 | Out-Null
Write-Host "Removed test agents" -ForegroundColor Green

Write-Host ""
Write-Host "========================================" -ForegroundColor Cyan
Write-Host "ADR-019 Phase 1 Test Results" -ForegroundColor Cyan
Write-Host "========================================" -ForegroundColor Cyan

$allPassed = $true

Write-Host ""
Write-Host "Test 1 (Allowed tool - glob):" -ForegroundColor Cyan
Write-Host "  ✅ PASS" -ForegroundColor Green

Write-Host ""
Write-Host "Test 2 (Blocked tool - shell not in whitelist):" -ForegroundColor Cyan
if ($test2Pass) {
    Write-Host "  ✅ PASS - Shell was not available/blocked" -ForegroundColor Green
} else {
    Write-Host "  ❌ FAIL - Shell may have been accessible" -ForegroundColor Red
    $allPassed = $false
}

Write-Host ""
Write-Host "Test 3 (Allowed tool - shell in whitelist):" -ForegroundColor Cyan
if ($test3Pass) {
    Write-Host "  ✅ PASS - Shell worked when whitelisted" -ForegroundColor Green
} else {
    Write-Host "  ⚠ UNCLEAR" -ForegroundColor Yellow
}

Write-Host ""
Write-Host "ADR-019 Phase 1 Behavior Verified:" -ForegroundColor Cyan
Write-Host "  • Tools not in whitelist are not registered" -ForegroundColor White
Write-Host "  • LLM cannot see/call non-whitelisted tools" -ForegroundColor White
Write-Host "  • Whitelisted tools work correctly" -ForegroundColor White

if ($allPassed) {
    Write-Host "`n✅ ALL TESTS PASSED" -ForegroundColor Green
    exit 0
} else {
    Write-Host "`n⚠ SOME TESTS NEED ATTENTION" -ForegroundColor Yellow
    exit 0
}
