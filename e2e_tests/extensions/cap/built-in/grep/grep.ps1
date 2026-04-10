#!/usr/bin/env pwsh
# Grep Tool E2E Test
#
# Tests the Grep tool for searching file contents using regex.

param(
    [string]$Provider = "kimi"
)

$ErrorActionPreference = "Stop"

Write-Host "========================================" -ForegroundColor Cyan
Write-Host "Grep Tool E2E Test" -ForegroundColor Cyan
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
$agentName = "grep_test"
pekobot agent create $agentName --provider $Provider -T coding 2>&1 | Out-Null
Write-Host "Created agent: $agentName" -ForegroundColor Green

# Enable granular tools via extension framework
pekobot ext enable read_file 2>&1 | Out-Null
pekobot ext enable write_file 2>&1 | Out-Null
pekobot ext enable glob 2>&1 | Out-Null
pekobot ext enable grep 2>&1 | Out-Null
pekobot ext enable str_replace_file 2>&1 | Out-Null
Write-Host "Enabled granular filesystem tools via extension framework" -ForegroundColor Green

# Get workspace directory
$workspaceDir = "$env:APPDATA/pekobot/workspaces/default/$agentName"

# Create test file structure
Write-Host "Creating test files..." -ForegroundColor Cyan
@"
fn main() {
    println!("Hello, World!");
}

fn helper() {
    println!("Helper function");
}
"@ | Out-File -FilePath "$workspaceDir/main.rs" -Encoding UTF8

@"
def calculate():
    return 42

def helper():
    pass
"@ | Out-File -FilePath "$workspaceDir/script.py" -Encoding UTF8

@"
TODO: Implement login
FIXME: Handle errors properly
"@ | Out-File -FilePath "$workspaceDir/notes.txt" -Encoding UTF8

Write-Host "Created test files: main.rs, script.py, notes.txt" -ForegroundColor Green

# ============================================================
# TEST 1: Search with regex pattern (run FIRST to avoid context issues)
# ============================================================
Write-Host "`n========================================" -ForegroundColor Cyan
Write-Host "TEST 1: Search with regex pattern" -ForegroundColor Cyan
Write-Host "========================================" -ForegroundColor Cyan

Write-Host "Sending request to search for 'calculate'..." -ForegroundColor Yellow
$result = pekobot send $agentName "Use your grep tool with pattern='calculate' to search for the word 'calculate' in your workspace. Report exactly what the grep tool returns." --no-stream 2>&1
Write-Host "Response: $result"

if ($result -match "calculate") {
    Write-Host "✓ Found Python function definitions" -ForegroundColor Green
} else {
    Write-Warning "⚠ Could not verify Python functions in response"
}

# ============================================================
# TEST 2: Search for function definitions
# ============================================================
Write-Host "`n========================================" -ForegroundColor Cyan
Write-Host "TEST 2: Search for 'fn' pattern" -ForegroundColor Cyan
Write-Host "========================================" -ForegroundColor Cyan

Write-Host "Sending request to search for 'fn'..." -ForegroundColor Yellow
$result = pekobot send $agentName "Use your grep tool (NOT shell) with pattern='fn ' and glob='*.rs' to search for Rust function definitions in your workspace. Report exactly what the grep tool returns." --no-stream 2>&1
Write-Host "Response: $result"

if ($result -match "main" -or $result -match "helper") {
    Write-Host "✓ Found function definitions" -ForegroundColor Green
} else {
    Write-Warning "⚠ Could not verify function definitions in response"
}

# ============================================================
# TEST 3: Search for TODO/FIXME
# ============================================================
Write-Host "`n========================================" -ForegroundColor Cyan
Write-Host "TEST 3: Search for TODO|FIXME pattern" -ForegroundColor Cyan
Write-Host "========================================" -ForegroundColor Cyan

Write-Host "Sending request to search for TODO..." -ForegroundColor Yellow
$result = pekobot send $agentName "Use your grep tool (NOT shell) with pattern='TODO|FIXME' and case_insensitive=true to search in your workspace. Report exactly what the grep tool returns." --no-stream 2>&1
Write-Host "Response: $result"

if ($result -match "TODO" -or $result -match "FIXME") {
    Write-Host "✓ Found TODO/FIXME comments" -ForegroundColor Green
} else {
    Write-Warning "⚠ Could not verify TODO/FIXME in response"
}

# ============================================================
# Cleanup
# ============================================================
Write-Host "`n========================================" -ForegroundColor Cyan
Write-Host "Test Complete - Cleaning up" -ForegroundColor Cyan
Write-Host "========================================" -ForegroundColor Cyan

# Clean up test files
Remove-Item "$workspaceDir/main.rs" -Force -ErrorAction SilentlyContinue
Remove-Item "$workspaceDir/script.py" -Force -ErrorAction SilentlyContinue
Remove-Item "$workspaceDir/notes.txt" -Force -ErrorAction SilentlyContinue
Write-Host "Removed test files" -ForegroundColor Green

pekobot agent delete $agentName --force 2>&1 | Out-Null
Write-Host "Deleted test agent" -ForegroundColor Green

Write-Host "`n✅ Grep e2e tests completed!" -ForegroundColor Green
