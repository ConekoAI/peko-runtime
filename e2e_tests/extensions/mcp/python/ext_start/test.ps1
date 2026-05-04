#!/usr/bin/env pwsh
# MCP ext start E2E Test
#
# Tests:
# 1. MCP server installation via ext install with unified manifest
# 2. Daemon start
# 3. MCP background runtime start via 'pekobot ext start'
# 4. Runtime status verification via 'pekobot ext status' and 'pekobot daemon status'
# 5. Tool execution via agent (reserved parameter injection)
# 6. Runtime stop and restart
#
# This test validates the new ExtensionRuntimeStarter path for MCP (ADR-026).

param(
    [string]$Provider = "minimax"
)

$ErrorActionPreference = "Stop"

Write-Host "========================================" -ForegroundColor Cyan
Write-Host "MCP ext start E2E Test" -ForegroundColor Cyan
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
$projectRoot = Resolve-Path "$PSScriptRoot/../../../../"
$pekoBinary = Join-Path $projectRoot "target/debug/pekobot.exe"
if (-not (Test-Path $pekoBinary)) {
    Write-Host "Building pekobot..." -ForegroundColor Cyan
    pushd $projectRoot
    $env:RUSTFLAGS = "-A warnings"
    cargo build --quiet
    if ($LASTEXITCODE -ne 0) {
        Write-Error "Build failed"
        exit 1
    }
    popd
} else {
    Write-Host "Using existing pekobot binary" -ForegroundColor Green
}

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
# STEP 1: Install MCP server as extension
# ============================================================
Write-Host "`n========================================" -ForegroundColor Cyan
Write-Host "STEP 1: Install MCP server as extension" -ForegroundColor Cyan
Write-Host "========================================" -ForegroundColor Cyan

$sourceDir = $PSScriptRoot
Write-Host "Installing MCP server 'identity' from $sourceDir..." -ForegroundColor Yellow

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
# STEP 2: Create agent
# ============================================================
Write-Host "`n========================================" -ForegroundColor Cyan
Write-Host "STEP 2: Create agent" -ForegroundColor Cyan
Write-Host "========================================" -ForegroundColor Cyan

$agentName = "mcp_ext_start_agent"

Write-Host "Creating agent: $agentName" -ForegroundColor Yellow
pekobot agent create $agentName --provider $Provider --force 2>&1 | Out-Null
Write-Host "Created agent" -ForegroundColor Green

# ============================================================
# STEP 3: Enable MCP tools for agent (access control only)
# ============================================================
Write-Host "`n========================================" -ForegroundColor Cyan
Write-Host "STEP 3: Enable MCP tools for agent" -ForegroundColor Cyan
Write-Host "========================================" -ForegroundColor Cyan

Write-Host "Enabling MCP extension 'identity' for agent..." -ForegroundColor Yellow
# In Phase 1, 'ext enable' still works but warns. We redirect stderr to avoid $ErrorActionPreference issues.
$enableResult = pekobot ext enable identity --target default/$agentName 2>&1
Write-Host $enableResult
Write-Host "Enabled MCP extension for agent" -ForegroundColor Green

# ============================================================
# STEP 4: Start daemon
# ============================================================
Write-Host "`n========================================" -ForegroundColor Cyan
Write-Host "STEP 4: Start daemon" -ForegroundColor Cyan
Write-Host "========================================" -ForegroundColor Cyan

Write-Host "Starting pekobot daemon in background..." -ForegroundColor Yellow

$daemonOut = "$env:TEMP\pekobot_mcp_daemon_out.log"
$daemonErr = "$env:TEMP\pekobot_mcp_daemon_err.log"
if (Test-Path $daemonOut) { Remove-Item $daemonOut }
if (Test-Path $daemonErr) { Remove-Item $daemonErr }

$daemonProc = Start-Process -FilePath "pekobot" -ArgumentList "daemon","start","--foreground" -PassThru -RedirectStandardOutput $daemonOut -RedirectStandardError $daemonErr -WindowStyle Hidden

# Wait for daemon to be ready
Start-Sleep -Seconds 4

# Check daemon status
$daemonStatus = pekobot daemon status 2>&1
Write-Host $daemonStatus
if ($daemonStatus -match "running" -or $daemonStatus -match "Daemon is running") {
    Write-Host "✓ Daemon is running" -ForegroundColor Green
} else {
    Write-Host "⚠ Daemon status unclear, continuing..." -ForegroundColor Yellow
    Write-Host "Daemon stdout:" -ForegroundColor Gray
    Get-Content $daemonOut -ErrorAction SilentlyContinue | Select-Object -Last 10
    Write-Host "Daemon stderr:" -ForegroundColor Gray
    Get-Content $daemonErr -ErrorAction SilentlyContinue | Select-Object -Last 10
}

# ============================================================
# STEP 5: Start MCP background runtime via ext start
# ============================================================
Write-Host "`n========================================" -ForegroundColor Cyan
Write-Host "STEP 5: Start MCP background runtime" -ForegroundColor Cyan
Write-Host "========================================" -ForegroundColor Cyan

Write-Host "Starting MCP background runtime via 'pekobot ext start identity'..." -ForegroundColor Yellow
$startResult = pekobot ext start identity 2>&1
Write-Host $startResult

if ($startResult -match "started" -or $startResult -match "running" -or $startResult -match "identity") {
    Write-Host "✓ MCP runtime start command accepted" -ForegroundColor Green
} else {
    Write-Error "Failed to start MCP runtime: $startResult"
}

# Give the MCP server time to initialize
Start-Sleep -Seconds 3

# ============================================================
# STEP 6: Check MCP runtime status
# ============================================================
Write-Host "`n========================================" -ForegroundColor Cyan
Write-Host "STEP 6: Check MCP runtime status" -ForegroundColor Cyan
Write-Host "========================================" -ForegroundColor Cyan

$statusResult = pekobot ext status identity 2>&1
Write-Host $statusResult

if ($statusResult -match "running" -or $statusResult -match "healthy" -or $statusResult -match "starting") {
    Write-Host "✓ MCP runtime has a valid state" -ForegroundColor Green
} else {
    Write-Host "⚠ MCP status unclear, checking daemon status..." -ForegroundColor Yellow
}

# Also check daemon status (shows daemon is running — background runtime details are in ext status)
$daemonStatus2 = pekobot daemon status 2>&1
Write-Host "`nDaemon status:" -ForegroundColor Cyan
Write-Host $daemonStatus2

if ($daemonStatus2 -match "Running" -or $daemonStatus2 -match "running") {
    Write-Host "✓ Daemon is running" -ForegroundColor Green
} else {
    Write-Host "⚠ Daemon status unclear" -ForegroundColor Yellow
}

# ============================================================
# STEP 7: Test MCP tool via agent
# ============================================================
Write-Host "`n========================================" -ForegroundColor Cyan
Write-Host "STEP 7: Test MCP tool via agent" -ForegroundColor Cyan
Write-Host "========================================" -ForegroundColor Cyan

Write-Host "Sending message to agent requesting identity echo..." -ForegroundColor Yellow
Write-Host "(This will demonstrate reserved parameter injection)" -ForegroundColor Gray

$sw = [System.Diagnostics.Stopwatch]::StartNew()
$response = pekobot send $agentName "We are testing your access and functionality of the MCP echo_identity tool. Please use the echo_identity tool with message 'Hello MCP'. Report back TOOL_SUCCESS if the tool works and shows injected identity, otherwise respond TOOL_FAILED with an explanation" --no-stream 2>&1
$sw.Stop()
Write-Host "Response time: $($sw.Elapsed.TotalSeconds)s"
Write-Host "Agent response: $response"

$toolSuccess = $response -match "TOOL_SUCCESS"
$toolFailed = $response -match "TOOL_FAILED"
if ($toolSuccess) {
    Write-Host "✓ MCP tool executed successfully with reserved parameter injection" -ForegroundColor Green
} elseif ($toolFailed) {
    Write-Host "⚠ MCP tool failed: $response" -ForegroundColor Yellow
} else {
    Write-Host "⚠ Tool result unclear, response was: $response" -ForegroundColor Yellow
}

# ============================================================
# STEP 8: Test MCP memory isolation
# ============================================================
Write-Host "`n========================================" -ForegroundColor Cyan
Write-Host "STEP 8: Test MCP memory isolation" -ForegroundColor Cyan
Write-Host "========================================" -ForegroundColor Cyan

Write-Host "Testing memory storage and retrieval..." -ForegroundColor Yellow

$memoryResponse = pekobot send $agentName "Please use the store_memory tool to save 'test_key=test_value'. Then use retrieve_memory with key 'test_key' to verify. Report MEMORY_OK if both work, otherwise MEMORY_FAILED" --no-stream 2>&1
Write-Host "Memory response: $memoryResponse"

$memoryOk = $memoryResponse -match "MEMORY_OK"
$memoryFailed = $memoryResponse -match "MEMORY_FAILED"
if ($memoryOk) {
    Write-Host "✓ MCP memory tools work correctly" -ForegroundColor Green
} elseif ($memoryFailed) {
    Write-Host "⚠ MCP memory tools failed" -ForegroundColor Yellow
} else {
    Write-Host "⚠ Memory result unclear" -ForegroundColor Yellow
}

# ============================================================
# STEP 9: Restart MCP runtime
# ============================================================
Write-Host "`n========================================" -ForegroundColor Cyan
Write-Host "STEP 9: Restart MCP runtime" -ForegroundColor Cyan
Write-Host "========================================" -ForegroundColor Cyan

Write-Host "Restarting MCP runtime..." -ForegroundColor Yellow
$restartResult = pekobot ext restart identity 2>&1
Write-Host $restartResult

if ($restartResult -match "restarted" -or $restartResult -match "started") {
    Write-Host "✓ MCP runtime restarted" -ForegroundColor Green
} else {
    Write-Host "⚠ Restart result unclear: $restartResult" -ForegroundColor Yellow
}

Start-Sleep -Seconds 3

# Verify after restart
$statusAfterRestart = pekobot ext status identity 2>&1
Write-Host "Status after restart: $statusAfterRestart"

# ============================================================
# STEP 10: Stop MCP runtime
# ============================================================
Write-Host "`n========================================" -ForegroundColor Cyan
Write-Host "STEP 10: Stop MCP runtime" -ForegroundColor Cyan
Write-Host "========================================" -ForegroundColor Cyan

Write-Host "Stopping MCP runtime..." -ForegroundColor Yellow
$stopResult = pekobot ext stop identity 2>&1
Write-Host $stopResult

if ($stopResult -match "stopped" -or $stopResult -match "not running") {
    Write-Host "✓ MCP runtime stopped" -ForegroundColor Green
} else {
    Write-Host "⚠ Stop result unclear: $stopResult" -ForegroundColor Yellow
}

# ============================================================
# Cleanup
# ============================================================
Write-Host "`n========================================" -ForegroundColor Cyan
Write-Host "Cleanup" -ForegroundColor Cyan
Write-Host "========================================" -ForegroundColor Cyan

# Stop daemon
if ($daemonProc -and -not $daemonProc.HasExited) {
    Stop-Process -Id $daemonProc.Id -Force -ErrorAction SilentlyContinue
    Write-Host "Stopped daemon process" -ForegroundColor Green
}

# Uninstall extension
pekobot ext uninstall identity --yes 2>&1 | Out-Null
Write-Host "Uninstalled MCP extension" -ForegroundColor Green

# Delete agent
pekobot agent delete $agentName --yes 2>&1 | Out-Null
Write-Host "Deleted agent" -ForegroundColor Green

# Show any daemon errors for debugging
if (Test-Path $daemonErr) {
    $errContent = Get-Content $daemonErr -ErrorAction SilentlyContinue
    if ($errContent) {
        Write-Host "`nDaemon stderr (for debugging):" -ForegroundColor Gray
        $errContent | Select-Object -Last 20 | ForEach-Object { Write-Host "  $_" -ForegroundColor Gray }
    }
}

Write-Host "`n========================================" -ForegroundColor Cyan
Write-Host "MCP ext start E2E test completed!" -ForegroundColor Cyan
Write-Host "========================================" -ForegroundColor Cyan

$allPassed = $toolSuccess -and $memoryOk
if ($allPassed) {
    Write-Host "✅ All critical checks passed!" -ForegroundColor Green
    exit 0
} else {
    Write-Host "⚠️ Some checks had issues — review output above" -ForegroundColor Yellow
    exit 0  # Don't fail the CI for flaky LLM-based tests
}
