#!/usr/bin/env pwsh
# Shell Tool E2E Test
#
# Tests the Shell tool for executing system commands.

param(
    [string]$Provider = "minimax"
)

$ErrorActionPreference = "Stop"

Write-Host "========================================" -ForegroundColor Cyan
Write-Host "Shell Tool E2E Test" -ForegroundColor Cyan
Write-Host "========================================" -ForegroundColor Cyan

# Check prerequisites
if (-not $env:MINIMAX_API_KEY -and $Provider -eq "minimax") {
    Write-Error "MINIMAX_API_KEY environment variable not set"
    exit 1
}

# Build pekobot
Write-Host "Building pekobot..." -ForegroundColor Cyan
pushd "$PSScriptRoot/../../.."
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
peko auth set $Provider $env:MINIMAX_API_KEY 2>&1 | Out-Null
Write-Host "Set API key for $Provider" -ForegroundColor Green

# Create agent
$agentName = "shell_test"
peko agent create $agentName --provider $Provider 2>&1 | Out-Null
Write-Host "Created agent: $agentName" -ForegroundColor Green

# Enable shell tool via extension framework
peko ext enable shell --target default/$agentName 2>&1 | Out-Null
Write-Host "Enabled shell tool via extension framework" -ForegroundColor Green

# Get workspace directory
$workspaceDir = "$env:APPDATA/pekobot/workspaces/default/$agentName"

# Create test subdirectory before tests (for TEST 2)
New-Item -ItemType Directory -Path "$workspaceDir/testdir" -Force | Out-Null
"test file" | Out-File -FilePath "$workspaceDir/testdir/test.txt" -Encoding UTF8
Write-Host "Created test subdirectory" -ForegroundColor Green

# ============================================================
# TEST 1: Basic shell command
# ============================================================
Write-Host "`n========================================" -ForegroundColor Cyan
Write-Host "TEST 1: Basic shell command (ls/dir)" -ForegroundColor Cyan
Write-Host "========================================" -ForegroundColor Cyan

$cmd = if ($IsWindows -or $env:OS -eq "Windows_NT") { "dir" } else { "ls" }
Write-Host "Sending request to execute $cmd..." -ForegroundColor Yellow
$response = peko send $agentName "Use your shell tool to check what's in your workspace. After executing the tool, respond TOOL_SUCCESS if you can see files listed, otherwise respond TOOL_FAILED." 2>&1
Start-Sleep -Seconds 3
Write-Host "Response: $response"

$toolSuccess = $response -match "TOOL_SUCCESS"
$toolFailed = $response -match "TOOL_FAILED"
if ($toolSuccess) {
    Write-Host "✅ PASS: Shell command executed successfully" -ForegroundColor Green
} elseif ($toolFailed) {
    Write-Host "❌ FAIL: Shell command did not work" -ForegroundColor Red
} else {
    Write-Host "⚠️ Result unclear" -ForegroundColor Yellow
}

# ============================================================
# TEST 2: Shell with working directory
# ============================================================
Write-Host "`n========================================" -ForegroundColor Cyan
Write-Host "TEST 2: Shell with different working directory" -ForegroundColor Cyan
Write-Host "========================================" -ForegroundColor Cyan

Write-Host "Sending request to list files in subdirectory..." -ForegroundColor Yellow
$response = peko send $agentName "Use your shell tool to check what's in testdir. After executing the tool, respond TOOL_SUCCESS if you see test.txt, otherwise respond TOOL_FAILED." --no-stream 2>&1
Start-Sleep -Seconds 3
Write-Host "Response: $response"

$toolSuccess = $response -match "TOOL_SUCCESS"
$toolFailed = $response -match "TOOL_FAILED"
if ($toolSuccess) {
    Write-Host "✅ PASS: Shell command with working directory executed successfully" -ForegroundColor Green
} elseif ($toolFailed) {
    Write-Host "❌ FAIL: Subdirectory shell command did not work" -ForegroundColor Red
} else {
    Write-Host "⚠️ Result unclear" -ForegroundColor Yellow
}

# ============================================================
# Cleanup
# ============================================================
Write-Host "`n========================================" -ForegroundColor Cyan
Write-Host "Test Complete - Cleaning up" -ForegroundColor Cyan
Write-Host "========================================" -ForegroundColor Cyan

# Clean up test files
if (Test-Path "$workspaceDir/testdir") {
    Remove-Item "$workspaceDir/testdir" -Recurse -Force
    Write-Host "Removed test directory" -ForegroundColor Green
}

peko agent delete $agentName --force 2>&1 | Out-Null
Write-Host "Deleted test agent" -ForegroundColor Green

Write-Host "`n✅ Shell tool e2e tests completed!" -ForegroundColor Green
