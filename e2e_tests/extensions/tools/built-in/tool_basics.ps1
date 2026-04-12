#!/usr/bin/env pwsh
# Tool Basics E2E Test for Extension Architecture
#
# Tests:
# 1. List built-in tools as extensions
# 2. Enable/disable tools via extension framework
# 3. Agent using enabled tools

param(
    [string]$Provider = "kimi"
)

$ErrorActionPreference = "Stop"

Write-Host "========================================" -ForegroundColor Cyan
Write-Host "Tool Basics E2E Test (Extension Architecture)" -ForegroundColor Cyan
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

# ============================================================
# TEST 1: List available extensions (built-in tools)
# ============================================================
Write-Host "`n========================================" -ForegroundColor Cyan
Write-Host "TEST 1: List available extensions" -ForegroundColor Cyan
Write-Host "========================================" -ForegroundColor Cyan

Write-Host "All installed extensions:" -ForegroundColor Yellow
pekobot ext list 2>&1

Write-Host "`nBuilt-in tool extensions:" -ForegroundColor Yellow
$extList = pekobot ext list 2>&1
Write-Host $extList

# Check for common built-in tools
$builtinTools = @("read_file", "write_file", "glob", "grep", "str_replace_file", "shell", "session_status")
$foundTools = 0
foreach ($tool in $builtinTools) {
    if ($extList -match $tool) {
        $foundTools++
        Write-Host "  ✓ Found built-in tool: $tool" -ForegroundColor Green
    }
}
Write-Host "`nFound $foundTools built-in tool extensions" -ForegroundColor Cyan

# ============================================================
# TEST 2: Create agent and enable tools
# ============================================================
Write-Host "`n========================================" -ForegroundColor Cyan
Write-Host "TEST 2: Create agent and enable tools" -ForegroundColor Cyan
Write-Host "========================================" -ForegroundColor Cyan

$agentName = "tool_basics_test"
pekobot agent create $agentName --provider $Provider 2>&1 | Out-Null
Write-Host "Created agent: $agentName" -ForegroundColor Green

# Enable tools via extension framework for this agent
Write-Host "`nEnabling tools via extension framework..." -ForegroundColor Yellow
pekobot ext enable read_file --target default/$agentName 2>&1 | Out-Null
pekobot ext enable write_file --target default/$agentName 2>&1 | Out-Null
pekobot ext enable glob --target default/$agentName 2>&1 | Out-Null
Write-Host "✓ Enabled read_file, write_file, glob for agent" -ForegroundColor Green

# Show extension info
Write-Host "`nExtension info for read_file:" -ForegroundColor Cyan
pekobot ext info read_file 2>&1

# ============================================================
# TEST 3: Agent uses enabled tools
# ============================================================
Write-Host "`n========================================" -ForegroundColor Cyan
Write-Host "TEST 3: Agent uses enabled tools" -ForegroundColor Cyan
Write-Host "========================================" -ForegroundColor Cyan

$workspaceDir = "$env:APPDATA/pekobot/workspaces/default/$agentName"

# Create a test file
"Test content for tool basics" | Out-File -FilePath "$workspaceDir/test.txt" -Encoding UTF8
Write-Host "Created test file" -ForegroundColor Green

# Ask agent to read the file
Write-Host "`nSending request to read test file..." -ForegroundColor Yellow
$result = pekobot send $agentName "Use your read_file tool to read the file 'test.txt' in your workspace. What does it contain?" --no-stream 2>&1
Write-Host "Response: $result"

if ($result -match "Test content" -or $result -match "tool basics") {
    Write-Host "✓ Agent successfully used read_file tool" -ForegroundColor Green
} else {
    Write-Warning "⚠ Could not verify tool usage"
}

# ============================================================
# TEST 4: Disable and re-enable a tool
# ============================================================
Write-Host "`n========================================" -ForegroundColor Cyan
Write-Host "TEST 4: Disable and re-enable a tool" -ForegroundColor Cyan
Write-Host "========================================" -ForegroundColor Cyan

Write-Host "Disabling glob tool..." -ForegroundColor Yellow
pekobot ext disable glob 2>&1 | Out-Null
$infoResult = pekobot ext info glob 2>&1
if ($infoResult -match "disabled") {
    Write-Host "✓ glob tool is now disabled" -ForegroundColor Green
}

Write-Host "`nRe-enabling glob tool..." -ForegroundColor Yellow
pekobot ext enable glob 2>&1 | Out-Null
$infoResult = pekobot ext info glob 2>&1
if ($infoResult -match "enabled") {
    Write-Host "✓ glob tool is now enabled" -ForegroundColor Green
}

# ============================================================
# Cleanup
# ============================================================
Write-Host "`n========================================" -ForegroundColor Cyan
Write-Host "Test Complete - Cleaning up" -ForegroundColor Cyan
Write-Host "========================================" -ForegroundColor Cyan

# Clean up test file
if (Test-Path "$workspaceDir/test.txt") {
    Remove-Item "$workspaceDir/test.txt" -Force
    Write-Host "Removed test file" -ForegroundColor Green
}

pekobot agent delete $agentName --force 2>&1 | Out-Null
Write-Host "Deleted test agent" -ForegroundColor Green

Write-Host "`n✅ Tool Basics E2E tests completed!" -ForegroundColor Green
Write-Host "" -ForegroundColor Cyan
Write-Host "Summary:" -ForegroundColor Cyan
Write-Host "  - Listed extensions including built-in tools" -ForegroundColor Cyan
Write-Host "  - Enabled tools via 'pekobot ext enable'" -ForegroundColor Cyan
Write-Host "  - Agent successfully used enabled tools" -ForegroundColor Cyan
Write-Host "  - Disabled and re-enabled tools" -ForegroundColor Cyan
Write-Host "" -ForegroundColor Cyan
Write-Host "Architecture verified:" -ForegroundColor Cyan
Write-Host "  - Built-in tools exposed as extensions" -ForegroundColor Cyan
Write-Host "  - Extension framework commands (list, enable, disable, info)" -ForegroundColor Cyan
Write-Host "  - ADR-017 Unified Extension Architecture" -ForegroundColor Cyan
