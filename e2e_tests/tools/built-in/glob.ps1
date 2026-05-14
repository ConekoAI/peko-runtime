#!/usr/bin/env pwsh
# Glob Tool E2E Test
#
# Tests the Glob tool for listing files matching patterns.

param(
    [string]$Provider = "minimax"
)

$ErrorActionPreference = "Stop"

Write-Host "========================================" -ForegroundColor Cyan
Write-Host "Glob Tool E2E Test" -ForegroundColor Cyan
Write-Host "========================================" -ForegroundColor Cyan

# Check prerequisites
if (-not $env:MINIMAX_API_KEY -and $Provider -eq "minimax") {
    Write-Error "MINIMAX_API_KEY environment variable not set"
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
peko auth set $Provider $env:MINIMAX_API_KEY 2>&1 | Out-Null
Write-Host "Set API key for $Provider" -ForegroundColor Green

# Create agent
$agentName = "glob_test"
peko agent create $agentName --provider $Provider 2>&1 | Out-Null
Write-Host "Created agent: $agentName" -ForegroundColor Green

# Enable granular tools via extension framework
peko ext enable glob --target default/$agentName 2>&1 | Out-Null
peko ext enable read_file --target default/$agentName 2>&1 | Out-Null
peko ext enable write_file --target default/$agentName 2>&1 | Out-Null
peko ext enable grep --target default/$agentName 2>&1 | Out-Null
peko ext enable str_replace_file --target default/$agentName 2>&1 | Out-Null
Write-Host "Enabled granular filesystem tools via extension framework" -ForegroundColor Green

# Get workspace directory
$workspaceDir = "$env:APPDATA/peko/workspaces/default/$agentName"

# Create test file structure
Write-Host "Creating test file structure..." -ForegroundColor Cyan
@"
Rust file one
"@ | Out-File -FilePath "$workspaceDir/file1.rs" -Encoding UTF8
@"
Rust file two
"@ | Out-File -FilePath "$workspaceDir/file2.rs" -Encoding UTF8
@"
Python file
"@ | Out-File -FilePath "$workspaceDir/script.py" -Encoding UTF8
New-Item -ItemType Directory -Path "$workspaceDir/src" -Force | Out-Null
@"
Source file
"@ | Out-File -FilePath "$workspaceDir/src/main.rs" -Encoding UTF8
Write-Host "Created test files: file1.rs, file2.rs, script.py, src/main.rs" -ForegroundColor Green

# ============================================================
# TEST 1: Glob *.py files
# ============================================================
Write-Host "`n========================================" -ForegroundColor Cyan
Write-Host "TEST 1: Glob *.py pattern" -ForegroundColor Cyan
Write-Host "========================================" -ForegroundColor Cyan

Write-Host "Sending request to find .py files..." -ForegroundColor Yellow
$response = peko send $agentName "Use your glob tool with pattern='*.py'. After getting the result, respond TOOL_SUCCESS if script.py is in the result, otherwise respond TOOL_FAILED with an explanation." --no-stream 2>&1
Start-Sleep -Seconds 3
Write-Host "Response: $response"

$toolSuccess = $response -match "TOOL_SUCCESS"
$toolFailed = $response -match "TOOL_FAILED"
if ($toolSuccess) {
    Write-Host "✅ PASS: Found .py files correctly" -ForegroundColor Green
} elseif ($toolFailed) {
    Write-Host "❌ FAIL: Glob did not find .py files" -ForegroundColor Red
} else {
    Write-Host "⚠️ Result unclear" -ForegroundColor Yellow
}

# ============================================================
# TEST 2: Glob *.rs files
# ============================================================
Write-Host "`n========================================" -ForegroundColor Cyan
Write-Host "TEST 2: Glob *.rs pattern" -ForegroundColor Cyan
Write-Host "========================================" -ForegroundColor Cyan

Write-Host "Sending request to find .rs files..." -ForegroundColor Yellow
$response = peko send $agentName "Use your glob tool (NOT shell) to find all files matching '*.rs' in your workspace. After getting the result, respond TOOL_SUCCESS if file1.rs is in the result, otherwise respond TOOL_FAILED with an explanation." --no-stream 2>&1
Start-Sleep -Seconds 3
Write-Host "Response: $response"

$toolSuccess = $response -match "TOOL_SUCCESS"
$toolFailed = $response -match "TOOL_FAILED"
if ($toolSuccess) {
    Write-Host "✅ PASS: Found .rs files correctly" -ForegroundColor Green
} elseif ($toolFailed) {
    Write-Host "❌ FAIL: Glob did not find .rs files" -ForegroundColor Red
} else {
    Write-Host "⚠️ Result unclear" -ForegroundColor Yellow
}

# ============================================================
# TEST 3: Glob **/*.rs (recursive)
# ============================================================
Write-Host "`n========================================" -ForegroundColor Cyan
Write-Host "TEST 3: Glob **/*.rs recursive pattern" -ForegroundColor Cyan
Write-Host "========================================" -ForegroundColor Cyan

Write-Host "Sending request to find all .rs files recursively..." -ForegroundColor Yellow
$response = peko send $agentName "Use your glob tool (NOT shell) with pattern='**/*.rs'. After getting the result, respond TOOL_SUCCESS if main.rs is in the result, otherwise respond TOOL_FAILED with an explanation." --no-stream 2>&1
Start-Sleep -Seconds 3
Write-Host "Response: $response"

$toolSuccess = $response -match "TOOL_SUCCESS"
$toolFailed = $response -match "TOOL_FAILED"
if ($toolSuccess) {
    Write-Host "✅ PASS: Found recursive .rs files correctly" -ForegroundColor Green
} elseif ($toolFailed) {
    Write-Host "❌ FAIL: Recursive glob did not work" -ForegroundColor Red
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
Remove-Item "$workspaceDir/file1.rs" -Force -ErrorAction SilentlyContinue
Remove-Item "$workspaceDir/file2.rs" -Force -ErrorAction SilentlyContinue
Remove-Item "$workspaceDir/script.py" -Force -ErrorAction SilentlyContinue
Remove-Item "$workspaceDir/src" -Recurse -Force -ErrorAction SilentlyContinue
Write-Host "Removed test files" -ForegroundColor Green

peko agent delete $agentName --force 2>&1 | Out-Null
Write-Host "Deleted test agent" -ForegroundColor Green

Write-Host "`n✅ Glob e2e tests completed!" -ForegroundColor Green
