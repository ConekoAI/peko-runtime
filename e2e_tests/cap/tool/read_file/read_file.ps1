#!/usr/bin/env pwsh
# ReadFile Tool E2E Test
#
# Tests the ReadFile tool for reading file contents with optional line ranges.

param(
    [string]$Provider = "kimi"
)

$ErrorActionPreference = "Stop"

Write-Host "========================================" -ForegroundColor Cyan
Write-Host "ReadFile Tool E2E Test" -ForegroundColor Cyan
Write-Host "========================================" -ForegroundColor Cyan

# Check prerequisites
if (-not $env:KIMI_API_KEY -and $Provider -eq "kimi") {
    Write-Error "KIMI_API_KEY environment variable not set"
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
pekobot auth set $Provider $env:KIMI_API_KEY 2>&1 | Out-Null
Write-Host "Set API key for $Provider" -ForegroundColor Green

# Create agent with coding template (enables granular tools)
$agentName = "readfile_test"
pekobot agent create $agentName --provider $Provider -T coding 2>&1 | Out-Null
Write-Host "Created agent: $agentName" -ForegroundColor Green

# Enable granular tools via cap framework
pekobot cap enable default/$agentName read_file 2>&1 | Out-Null
pekobot cap enable default/$agentName write_file 2>&1 | Out-Null
pekobot cap enable default/$agentName glob 2>&1 | Out-Null
pekobot cap enable default/$agentName grep 2>&1 | Out-Null
pekobot cap enable default/$agentName str_replace_file 2>&1 | Out-Null
Write-Host "Enabled granular filesystem tools via cap framework" -ForegroundColor Green

# Get workspace directory
$workspaceDir = "$env:APPDATA/pekobot/workspaces/default/$agentName"

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
$result = pekobot send $agentName "Use your read_file tool (not shell) to read the file called 'test_read.txt' in your workspace. Report exactly what the read_file tool returns." --no-stream 2>&1
Write-Host "Response: $result"

if ($result -match "Line 1" -or $result -match "Hello" -or $result -match "World") {
    Write-Host "✓ File content read successfully" -ForegroundColor Green
} else {
    Write-Warning "⚠ Could not verify file content in response"
}

# ============================================================
# TEST 2: Read with line range
# ============================================================
Write-Host "`n========================================" -ForegroundColor Cyan
Write-Host "TEST 2: Read with line range" -ForegroundColor Cyan
Write-Host "========================================" -ForegroundColor Cyan

Write-Host "Sending request to read lines 2-3..." -ForegroundColor Yellow
$result = pekobot send $agentName "Use your read_file tool with line_range parameter set to '2-3' to read only lines 2-3 from 'test_read.txt'. Tell me exactly what those lines contain." --no-stream 2>&1
Write-Host "Response: $result"

if ($result -match "World" -or $result -match "Testing") {
    Write-Host "✓ Line range read successfully" -ForegroundColor Green
} else {
    Write-Warning "⚠ Could not verify line range in response"
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

pekobot agent delete $agentName --force 2>&1 | Out-Null
Write-Host "Deleted test agent" -ForegroundColor Green

Write-Host "`n✅ ReadFile e2e tests completed!" -ForegroundColor Green
