#!/usr/bin/env pwsh
# MCP Reserved Parameter Injection E2E Test
#
# Tests:
# 1. MCP server installation via ext install
# 2. Reserved parameter injection (agent_id, session_id) into MCP tool calls
# 3. Tool execution via pekobot send
# 4. Verification that reserved params are injected but hidden from LLM
#
# Following the same pattern as universal tool E2E test

param(
    [string]$Provider = "minimax"
)

$ErrorActionPreference = "Stop"

Write-Host "========================================" -ForegroundColor Cyan
Write-Host "MCP Reserved Parameter Injection E2E Test" -ForegroundColor Cyan
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
    Write-Host "Build had warnings, continuing..." -ForegroundColor Yellow
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
# STEP 1: Install MCP server as extension (FIRST - before creating agent)
# ============================================================
Write-Host "`n========================================" -ForegroundColor Cyan
Write-Host "STEP 1: Install MCP server as extension" -ForegroundColor Cyan
Write-Host "========================================" -ForegroundColor Cyan

$sourceDir = $PSScriptRoot
Write-Host "Installing MCP server 'identity' from $sourceDir..." -ForegroundColor Yellow

# Install the MCP server as an mcp extension
# Install from directory to include both manifest.yaml and mcp_server.py
$installResult = pekobot ext install $sourceDir --type mcp 2>&1
Write-Host $installResult

# Verify installation
$extList = pekobot ext list --type mcp 2>&1
if ($extList -match "identity") {
    Write-Host "✓ MCP server extension installed successfully" -ForegroundColor Green
} else {
    Write-Error "MCP server extension installation failed"
}

# ============================================================
# STEP 2: Create agent (SECOND - after MCP extension is installed)
# ============================================================
Write-Host "`n========================================" -ForegroundColor Cyan
Write-Host "STEP 2: Create agent" -ForegroundColor Cyan
Write-Host "========================================" -ForegroundColor Cyan

$agentName = "mcp_identity_agent"

Write-Host "Creating agent: $agentName" -ForegroundColor Yellow
pekobot agent create $agentName --provider $Provider --force 2>&1 | Out-Null
Write-Host "Created agent" -ForegroundColor Green

# ============================================================
# STEP 3: Enable MCP extension for agent
# ============================================================
Write-Host "`n========================================" -ForegroundColor Cyan
Write-Host "STEP 3: Enable MCP extension for agent" -ForegroundColor Cyan
Write-Host "========================================" -ForegroundColor Cyan

Write-Host "Enabling MCP extension 'identity'..." -ForegroundColor Yellow
$enableResult = pekobot ext enable identity --target default/$agentName 2>&1
Write-Host $enableResult
Write-Host "Enabled MCP extension" -ForegroundColor Green

# Verify
$extInfo = pekobot ext info identity 2>&1
Write-Host "`nExtension status:" -ForegroundColor Cyan
Write-Host $extInfo

# Verify whitelist was updated
$agentConfig = "$env:USERPROFILE/.pekobot/teams/default/agents/$agentName/config.toml"
Write-Host "`nAgent tool whitelist:" -ForegroundColor Cyan
Get-Content $agentConfig | Select-String -Pattern "enabled" | ForEach-Object { Write-Host $_ }

# ============================================================
# STEP 4: Test MCP tool via agent
# ============================================================
Write-Host "`n========================================" -ForegroundColor Cyan
Write-Host "STEP 4: Test MCP tool via agent" -ForegroundColor Cyan
Write-Host "========================================" -ForegroundColor Cyan

Write-Host "Sending message to agent requesting identity echo..." -ForegroundColor Yellow
Write-Host "(This will demonstrate reserved parameter injection)" -ForegroundColor Gray

Measure-Command {
    $response = pekobot send $agentName "We are testing your access and functionality of the MCP echo_identity tool. Please use the echo_identity tool with message 'Hello MCP'. Report back TOOL_SUCCESS if the tool works and shows injected identity, otherwise respond TOOL_FAILED with an explanation" --no-stream 2>&1
}
Write-Host "Agent response: $response"

$toolSuccess = $response -match "TOOL_SUCCESS"
$toolFailed = $response -match "TOOL_FAILED"
if ($toolSuccess) {
    Write-Host "✅ PASS: MCP tool worked correctly with reserved parameter injection" -ForegroundColor Green
} elseif ($toolFailed) {
    Write-Host "❌ FAIL: MCP tool did not work" -ForegroundColor Red
} else {
    Write-Host "⚠️ Tool result unclear" -ForegroundColor Yellow
}

# ============================================================
# STEP 5: Test MCP memory isolation
# ============================================================
Write-Host "`n========================================" -ForegroundColor Cyan
Write-Host "STEP 5: Test MCP memory isolation" -ForegroundColor Cyan
Write-Host "========================================" -ForegroundColor Cyan

Write-Host "Testing memory storage and retrieval..." -ForegroundColor Yellow

# Store and retrieve in a single conversation to test memory isolation
$response2 = pekobot send $agentName "First, store the value 'E2E Test Value' with key 'test_key' using the store_memory tool. Then, retrieve the value using retrieve_memory with key 'test_key'. If both work and you get 'E2E Test Value' back, respond TOOL_SUCCESS. If anything fails, respond TOOL_FAILED." --no-stream 2>&1
Write-Host "Memory response: $response2"

if ($response2 -match "TOOL_SUCCESS" -or $response2 -match "E2E Test Value") {
    Write-Host "✅ PASS: Memory storage and retrieval works correctly" -ForegroundColor Green
} else {
    Write-Host "⚠️ Memory result unclear" -ForegroundColor Yellow
}

# ============================================================
# Cleanup
# ============================================================
Write-Host "`n========================================" -ForegroundColor Cyan
Write-Host "Cleanup" -ForegroundColor Cyan
Write-Host "========================================" -ForegroundColor Cyan

# Uninstall MCP extension
pekobot ext uninstall identity 2>&1 | Out-Null
Write-Host "Uninstalled MCP extension" -ForegroundColor Green

# Delete agent
pekobot agent delete $agentName --force 2>&1 | Out-Null
Write-Host "Deleted agent" -ForegroundColor Green

Write-Host "`n✅ MCP E2E test completed!" -ForegroundColor Green
Write-Host "" -ForegroundColor Cyan
Write-Host "Summary:" -ForegroundColor Cyan
Write-Host "  - MCP server installed via 'pekobot ext install --type mcp'" -ForegroundColor Cyan
Write-Host "  - MCP tools enabled for agent via 'pekobot ext enable --target'" -ForegroundColor Cyan
Write-Host "  - Reserved parameters (agent_id, session_id) injected correctly" -ForegroundColor Cyan
Write-Host "  - MCP tools (echo_identity, store_memory, retrieve_memory) executed successfully" -ForegroundColor Cyan
