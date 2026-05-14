#!/usr/bin/env pwsh
# ReadFile Tool E2E Test
#
# Tests the ReadFile tool for reading file contents with optional line ranges.

param(
    [string]$Provider = "minimax"
)

$ErrorActionPreference = "Stop"

Write-Host "========================================" -ForegroundColor Cyan
Write-Host "ReadFile Tool E2E Test" -ForegroundColor Cyan
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
$agentName = "readfile_test"
peko agent create $agentName --provider $Provider 2>&1 | Out-Null
Write-Host "Created agent: $agentName" -ForegroundColor Green

# Enable granular tools via extension framework
peko ext enable read_file --target default/$agentName 2>&1 | Out-Null
peko ext enable write_file --target default/$agentName 2>&1 | Out-Null
peko ext enable glob --target default/$agentName 2>&1 | Out-Null
peko ext enable grep --target default/$agentName 2>&1 | Out-Null
peko ext enable str_replace_file --target default/$agentName 2>&1 | Out-Null
Write-Host "Enabled granular filesystem tools via extension framework" -ForegroundColor Green

# Get workspace directory
$workspaceDir = "$env:APPDATA/peko/workspaces/default/$agentName"

# Create test file in workspace
$testFile = "$workspaceDir/test_read.txt"
$testContent = @"
Line 1: Hello
Line 2: World
Line 3: Testing
Line 4: ReadFile
Line 5: Tool
"@
New-Item -ItemType File -Path $testFile -Force | Out-Null
Set-Content -Path $testFile -Value $testContent -Encoding UTF8
Write-Host "Created test file: $testFile" -ForegroundColor Green

# ============================================================
# TEST 1: Read entire file
# ============================================================
Write-Host "`n========================================" -ForegroundColor Cyan
Write-Host "TEST 1: Read entire file" -ForegroundColor Cyan
Write-Host "========================================" -ForegroundColor Cyan

Write-Host "Sending request to read the test file..." -ForegroundColor Yellow
$response = peko send $agentName "Use your read_file tool (not shell) to read the file called 'test_read.txt' in your workspace. After reading, respond TOOL_SUCCESS if you see 'Line 1: Hello', otherwise respond TOOL_FAILED." --no-stream 2>&1
Start-Sleep -Seconds 3
Write-Host "Response: $response"

$toolSuccess = $response -match "TOOL_SUCCESS"
$toolFailed = $response -match "TOOL_FAILED"
if ($toolSuccess) {
    Write-Host "✅ PASS: File content read successfully" -ForegroundColor Green
} elseif ($toolFailed) {
    Write-Host "❌ FAIL: ReadFile did not work" -ForegroundColor Red
} else {
    Write-Host "⚠️ Result unclear" -ForegroundColor Yellow
}

# ============================================================
# TEST 2: Read with line range
# ============================================================
Write-Host "`n========================================" -ForegroundColor Cyan
Write-Host "TEST 2: Read with line range" -ForegroundColor Cyan
Write-Host "========================================" -ForegroundColor Cyan

Write-Host "Sending request to read lines 2-3..." -ForegroundColor Yellow
$response = peko send $agentName "Use your read_file tool with line_range parameter set to '2-3' to read only lines 2-3 from 'test_read.txt'. After reading, respond TOOL_SUCCESS if you see 'Line 2: World' and 'Line 3: Testing', otherwise respond TOOL_FAILED." --no-stream 2>&1
Start-Sleep -Seconds 3
Write-Host "Response: $response"

$toolSuccess = $response -match "TOOL_SUCCESS"
$toolFailed = $response -match "TOOL_FAILED"
if ($toolSuccess) {
    Write-Host "✅ PASS: Line range read successfully" -ForegroundColor Green
} elseif ($toolFailed) {
    Write-Host "❌ FAIL: Line range read did not work" -ForegroundColor Red
} else {
    Write-Host "⚠️ Result unclear" -ForegroundColor Yellow
}

# ============================================================
# Cleanup
# ============================================================
Write-Host "`n========================================" -ForegroundColor Cyan
Write-Host "Test Complete - Cleaning up" -ForegroundColor Cyan
Write-Host "========================================" -ForegroundColor Cyan

# Clean up test file
if (Test-Path $testFile) {
    Remove-Item $testFile -Force
    Write-Host "Removed test file" -ForegroundColor Green
}

peko agent delete $agentName --force 2>&1 | Out-Null
Write-Host "Deleted test agent" -ForegroundColor Green

Write-Host "`n✅ ReadFile e2e tests completed!" -ForegroundColor Green
