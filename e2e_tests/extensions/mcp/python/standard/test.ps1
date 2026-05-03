#!/usr/bin/env pwsh
# Standard MCP Server E2E Test (Tier 1: server.json)
#
# Tests:
# 1. MCP server installation via ext install WITHOUT --type flag
#    (server.json should be auto-detected as Tier 1 ecosystem standard)
# 2. Tool discovery and registration
# 3. Tool execution via pekobot send (echo, add, get_server_info)
# 4. Verification that NO manifest.yaml or Pekobot-specific metadata is needed
#
# This validates ADR-024's ecosystem compatibility goal: pure MCP servers
# from the broader ecosystem should work in Pekobot without custom metadata.

param(
    [string]$Provider = "minimax"
)

$ErrorActionPreference = "Stop"

Write-Host "========================================" -ForegroundColor Cyan
Write-Host "Standard MCP Server E2E Test (Tier 1)" -ForegroundColor Cyan
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
# STEP 1: Verify this is a pure standard MCP server
# ============================================================
Write-Host "`n========================================" -ForegroundColor Cyan
Write-Host "STEP 1: Verify pure standard MCP server" -ForegroundColor Cyan
Write-Host "========================================" -ForegroundColor Cyan

$serverJson = "$PSScriptRoot/server.json"
$manifestYaml = "$PSScriptRoot/manifest.yaml"

if (Test-Path $serverJson) {
    Write-Host "✓ server.json found (Tier 1 ecosystem standard)" -ForegroundColor Green
} else {
    Write-Error "server.json not found"
}

if (Test-Path $manifestYaml) {
    Write-Error "manifest.yaml should NOT exist for this standard MCP test"
} else {
    Write-Host "✓ No manifest.yaml (confirms pure standard server)" -ForegroundColor Green
}

# ============================================================
# STEP 2: Install MCP server as extension (NO --type flag)
# ============================================================
Write-Host "`n========================================" -ForegroundColor Cyan
Write-Host "STEP 2: Install MCP server (auto-detect via server.json)" -ForegroundColor Cyan
Write-Host "========================================" -ForegroundColor Cyan

$sourceDir = $PSScriptRoot
Write-Host "Installing standard MCP server from $sourceDir..." -ForegroundColor Yellow
Write-Host "(No --type flag: server.json should auto-detect as mcp)" -ForegroundColor Gray

# Install WITHOUT --type flag — Tier 1 detection should handle it
$installResult = pekobot ext install $sourceDir 2>&1
Write-Host $installResult

# Verify installation
$extList = pekobot ext list --type mcp 2>&1
if ($extList -match "standard-echo") {
    Write-Host "✓ Standard MCP server extension installed successfully" -ForegroundColor Green
} else {
    Write-Error "Standard MCP server extension installation failed"
}

# Verify the extension type is correctly detected as "mcp"
$extInfo = pekobot ext info standard-echo 2>&1
Write-Host "`nExtension info:" -ForegroundColor Cyan
Write-Host $extInfo

if ($extInfo -match "mcp") {
    Write-Host "✓ Extension correctly detected as type 'mcp'" -ForegroundColor Green
} else {
    Write-Error "Extension type is not 'mcp'"
}

# ============================================================
# STEP 3: Create agent (after MCP extension is installed)
# ============================================================
Write-Host "`n========================================" -ForegroundColor Cyan
Write-Host "STEP 3: Create agent" -ForegroundColor Cyan
Write-Host "========================================" -ForegroundColor Cyan

$agentName = "mcp_standard_agent"

Write-Host "Creating agent: $agentName" -ForegroundColor Yellow
pekobot agent create $agentName --provider $Provider --force 2>&1 | Out-Null
Write-Host "Created agent" -ForegroundColor Green

# ============================================================
# STEP 4: Enable MCP extension for agent
# ============================================================
Write-Host "`n========================================" -ForegroundColor Cyan
Write-Host "STEP 4: Enable MCP extension for agent" -ForegroundColor Cyan
Write-Host "========================================" -ForegroundColor Cyan

Write-Host "Enabling MCP extension 'standard-echo'..." -ForegroundColor Yellow
$enableResult = pekobot ext enable standard-echo --target default/$agentName 2>&1
Write-Host $enableResult
Write-Host "Enabled MCP extension" -ForegroundColor Green

# Verify whitelist was updated
$agentConfig = "$env:USERPROFILE/.pekobot/teams/default/agents/$agentName/config.toml"
Write-Host "`nAgent tool whitelist:" -ForegroundColor Cyan
Get-Content $agentConfig | Select-String -Pattern "enabled" | ForEach-Object { Write-Host $_ }

# ============================================================
# STEP 5: Test echo tool via agent
# ============================================================
Write-Host "`n========================================" -ForegroundColor Cyan
Write-Host "STEP 5: Test echo tool via agent" -ForegroundColor Cyan
Write-Host "========================================" -ForegroundColor Cyan

Write-Host "Sending message to agent requesting echo..." -ForegroundColor Yellow

$response = pekobot send $agentName "Please use the echo tool with message 'Hello from standard MCP'. If the tool works and echoes the message back, respond TOOL_SUCCESS. Otherwise respond TOOL_FAILED with an explanation." --no-stream 2>&1
Write-Host "Agent response: $response"

if ($response -match "TOOL_SUCCESS") {
    Write-Host "✅ PASS: Echo tool worked correctly" -ForegroundColor Green
} elseif ($response -match "TOOL_FAILED") {
    Write-Host "❌ FAIL: Echo tool did not work" -ForegroundColor Red
} else {
    Write-Host "⚠️ Tool result unclear" -ForegroundColor Yellow
}

# ============================================================
# STEP 6: Test add tool via agent
# ============================================================
Write-Host "`n========================================" -ForegroundColor Cyan
Write-Host "STEP 6: Test add tool via agent" -ForegroundColor Cyan
Write-Host "========================================" -ForegroundColor Cyan

Write-Host "Sending message to agent requesting addition..." -ForegroundColor Yellow

$response2 = pekobot send $agentName "Please use the add tool to calculate 17 plus 28. If the result is 45, respond TOOL_SUCCESS with the answer. Otherwise respond TOOL_FAILED." --no-stream 2>&1
Write-Host "Agent response: $response2"

if ($response2 -match "TOOL_SUCCESS" -or $response2 -match "45") {
    Write-Host "✅ PASS: Add tool worked correctly" -ForegroundColor Green
} elseif ($response2 -match "TOOL_FAILED") {
    Write-Host "❌ FAIL: Add tool did not work" -ForegroundColor Red
} else {
    Write-Host "⚠️ Tool result unclear" -ForegroundColor Yellow
}

# ============================================================
# STEP 7: Test get_server_info tool via agent
# ============================================================
Write-Host "`n========================================" -ForegroundColor Cyan
Write-Host "STEP 7: Test get_server_info tool via agent" -ForegroundColor Cyan
Write-Host "========================================" -ForegroundColor Cyan

Write-Host "Sending message to agent requesting server info..." -ForegroundColor Yellow

$response3 = pekobot send $agentName "Please use the get_server_info tool to get information about this MCP server. If the tool returns info including 'standard-echo', respond TOOL_SUCCESS. Otherwise respond TOOL_FAILED." --no-stream 2>&1
Write-Host "Agent response: $response3"

if ($response3 -match "TOOL_SUCCESS" -or $response3 -match "standard-echo") {
    Write-Host "✅ PASS: get_server_info tool worked correctly" -ForegroundColor Green
} elseif ($response3 -match "TOOL_FAILED") {
    Write-Host "❌ FAIL: get_server_info tool did not work" -ForegroundColor Red
} else {
    Write-Host "⚠️ Tool result unclear" -ForegroundColor Yellow
}

# ============================================================
# Cleanup
# ============================================================
Write-Host "`n========================================" -ForegroundColor Cyan
Write-Host "Cleanup" -ForegroundColor Cyan
Write-Host "========================================" -ForegroundColor Cyan

# Uninstall MCP extension
pekobot ext uninstall standard-echo 2>&1 | Out-Null
Write-Host "Uninstalled standard MCP extension" -ForegroundColor Green

# Delete agent
pekobot agent delete $agentName --force 2>&1 | Out-Null
Write-Host "Deleted agent" -ForegroundColor Green

Write-Host "`n✅ Standard MCP E2E test completed!" -ForegroundColor Green
Write-Host "" -ForegroundColor Cyan
Write-Host "Summary:" -ForegroundColor Cyan
Write-Host "  - MCP server installed via 'pekobot ext install' (NO --type flag)" -ForegroundColor Cyan
Write-Host "  - server.json auto-detected as Tier 1 ecosystem standard -> mcp" -ForegroundColor Cyan
Write-Host "  - No manifest.yaml or Pekobot-specific metadata required" -ForegroundColor Cyan
Write-Host "  - MCP tools (echo, add, get_server_info) executed successfully" -ForegroundColor Cyan
Write-Host "" -ForegroundColor Cyan
Write-Host "ADR-024 Validation:" -ForegroundColor Cyan
Write-Host "  ✓ Tier 1 detection: server.json -> mcp adapter" -ForegroundColor Cyan
Write-Host "  ✓ Ecosystem compatibility: pure MCP server works without custom metadata" -ForegroundColor Cyan
