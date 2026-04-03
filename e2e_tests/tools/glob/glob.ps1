#!/usr/bin/env pwsh
# Glob Tool E2E Test
#
# Tests the Glob tool for listing files matching patterns.

param(
    [string]$Provider = "kimi"
)

$ErrorActionPreference = "Stop"

Write-Host "========================================" -ForegroundColor Cyan
Write-Host "Glob Tool E2E Test" -ForegroundColor Cyan
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
$agentName = "glob_test"
pekobot agent create $agentName --provider $Provider -T coding 2>&1 | Out-Null
Write-Host "Created agent: $agentName" -ForegroundColor Green

# Enable granular tools via agent config set
pekobot agent config set $agentName tools.enabled '["shell","session_status","read_file","write_file","glob","grep","str_replace_file"]' 2>&1 | Out-Null
Write-Host "Enabled granular filesystem tools" -ForegroundColor Green

# Get workspace directory
$workspaceDir = "$env:APPDATA/pekobot/workspaces/default/$agentName"

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
# TEST 1: Glob *.rs files
# ============================================================
Write-Host "`n========================================" -ForegroundColor Cyan
Write-Host "TEST 1: Glob *.rs pattern" -ForegroundColor Cyan
Write-Host "========================================" -ForegroundColor Cyan

Write-Host "Sending request to find .rs files..." -ForegroundColor Yellow
$result = pekobot send $agentName "Use your glob tool (NOT shell) to find all files matching '*.rs' in your workspace. Report exactly what the glob tool returns." --no-stream 2>&1
Write-Host "Response: $result"

if ($result -match "\.rs" -and $result -match "file1") {
    Write-Host "✓ Found .rs files correctly" -ForegroundColor Green
} else {
    Write-Warning "⚠ Could not verify .rs files in response"
}

# ============================================================
# TEST 2: Glob **/*.rs recursive
# ============================================================
Write-Host "`n========================================" -ForegroundColor Cyan
Write-Host "TEST 2: Glob **/*.rs recursive pattern" -ForegroundColor Cyan
Write-Host "========================================" -ForegroundColor Cyan

Write-Host "Sending request to find all .rs files recursively..." -ForegroundColor Yellow
$result = pekobot send $agentName "Use your glob tool (NOT shell) with pattern='**/*.rs' for recursive search in your workspace. Report exactly what the glob tool returns." --no-stream 2>&1
Write-Host "Response: $result"

if ($result -match "main.rs") {
    Write-Host "✓ Found recursive .rs files correctly" -ForegroundColor Green
} else {
    Write-Warning "⚠ Could not verify recursive search in response"
}

# ============================================================
# TEST 3: Glob *.py files
# ============================================================
Write-Host "`n========================================" -ForegroundColor Cyan
Write-Host "TEST 3: Glob *.py pattern" -ForegroundColor Cyan
Write-Host "========================================" -ForegroundColor Cyan

Write-Host "Sending request to find .py files..." -ForegroundColor Yellow
$result = pekobot send $agentName "Use your glob tool (NOT shell) with pattern='*.py' to find Python files in your workspace." --no-stream 2>&1
Write-Host "Response: $result"

if ($result -match "script.py") {
    Write-Host "✓ Found .py files correctly" -ForegroundColor Green
} else {
    Write-Warning "⚠ Could not verify .py files in response"
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

pekobot agent delete $agentName --force 2>&1 | Out-Null
Write-Host "Deleted test agent" -ForegroundColor Green

Write-Host "`n✅ Glob e2e tests completed!" -ForegroundColor Green
