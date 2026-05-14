#!/usr/bin/env pwsh
# Shell Tool E2E Test
#
# Tests the Shell tool for executing system commands.

param(
    [string]$Provider = "kimi"
)

$ErrorActionPreference = "Stop"

Write-Host "========================================" -ForegroundColor Cyan
Write-Host "Shell Tool E2E Test" -ForegroundColor Cyan
Write-Host "========================================" -ForegroundColor Cyan

# Check prerequisites
if (-not $env:KIMI_API_KEY -and $Provider -eq "kimi") {
    Write-Error "KIMI_API_KEY environment variable not set"
    exit 1
}

# Build peko
Write-Host "Building peko..." -ForegroundColor Cyan
pushd "$PSScriptRoot/../../.."
$env:RUSTFLAGS = "-A warnings"
cargo build --quiet
if ($LASTEXITCODE -ne 0) {
    Write-Error "Build failed"
    exit 1
}
popd

# Reset peko config data
$pekoDir = "$env:USERPROFILE/.peko"
if (Test-Path $pekoDir) {
    Remove-Item -Recurse -Force $pekoDir
    Write-Host "Reset .peko directory" -ForegroundColor Yellow
}
$DataDir = "$env:APPDATA/peko"
if (Test-Path $DataDir) {
    Remove-Item -Recurse -Force $DataDir
    Write-Host "Reset data directory" -ForegroundColor Yellow
}

# Set API key
peko auth set $Provider $env:KIMI_API_KEY 2>&1 | Out-Null
Write-Host "Set API key for $Provider" -ForegroundColor Green

# Create agent with coding template
$agentName = "shell_test"
peko agent create $agentName --provider $Provider 2>&1 | Out-Null
Write-Host "Created agent: $agentName" -ForegroundColor Green

# Enable shell tool via extension framework
peko ext enable shell 2>&1 | Out-Null
Write-Host "Enabled shell tool via extension framework" -ForegroundColor Green

# Get workspace directory
$workspaceDir = "$env:APPDATA/peko/workspaces/default/$agentName"

# ============================================================
# TEST 1: Basic shell command
# ============================================================
Write-Host "`n========================================" -ForegroundColor Cyan
Write-Host "TEST 1: Basic shell command (ls/dir)" -ForegroundColor Cyan
Write-Host "========================================" -ForegroundColor Cyan

$cmd = if ($IsWindows -or $env:OS -eq "Windows_NT") { "dir" } else { "ls" }
Write-Host "Sending request to execute $cmd..." -ForegroundColor Yellow
$result = peko send $agentName "Use your shell tool to run '$cmd' in your workspace. Report the output." --no-stream 2>&1
Write-Host "Response: $result"

if ($result -match "Directory" -or $result -match "total" -or $result -match "file") {
    Write-Host "✓ Shell command executed successfully" -ForegroundColor Green
} else {
    Write-Warning "⚠ Could not verify shell output"
}

# ============================================================
# TEST 2: Shell with working directory
# ============================================================
Write-Host "`n========================================" -ForegroundColor Cyan
Write-Host "TEST 2: Shell with different working directory" -ForegroundColor Cyan
Write-Host "========================================" -ForegroundColor Cyan

# Create a subdirectory
New-Item -ItemType Directory -Path "$workspaceDir/testdir" -Force | Out-Null
"test file" | Out-File -FilePath "$workspaceDir/testdir/test.txt" -Encoding UTF8

Write-Host "Sending request to list files in subdirectory..." -ForegroundColor Yellow
$result = peko send $agentName "Use your shell tool to run '$cmd' in the 'testdir' subdirectory. Report what files you see." --no-stream 2>&1
Write-Host "Response: $result"

if ($result -match "test.txt") {
    Write-Host "✓ Shell command with working directory executed successfully" -ForegroundColor Green
} else {
    Write-Warning "⚠ Could not verify subdirectory listing"
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
