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

# Enable granular tools in agent config
pekobot agent config set $agentName tools.enabled '["shell", "session_status", "ReadFile", "WriteFile", "Glob", "Grep", "StrReplaceFile"]' 2>&1 | Out-Null
Write-Host "Enabled granular filesystem tools" -ForegroundColor Green

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
# TEST 1: Search for function definitions
# ============================================================
Write-Host "`n========================================" -ForegroundColor Cyan
Write-Host "TEST 1: Search for 'fn' pattern" -ForegroundColor Cyan
Write-Host "========================================" -ForegroundColor Cyan

Write-Host "Sending request to search for 'fn'..." -ForegroundColor Yellow
$result = pekobot send $agentName "Use Grep to search for 'fn ' (function definition) in '*.rs' files in your workspace. Report what functions you find." --no-stream 2>&1
Write-Host "Response: $result"

if ($result -match "main" -or $result -match "helper") {
    Write-Host "✓ Found function definitions" -ForegroundColor Green
} else {
    Write-Warning "⚠ Could not verify function definitions in response"
}

# ============================================================
# TEST 2: Search for TODO/FIXME
# ============================================================
Write-Host "`n========================================" -ForegroundColor Cyan
Write-Host "TEST 2: Search for TODO|FIXME pattern" -ForegroundColor Cyan
Write-Host "========================================" -ForegroundColor Cyan

Write-Host "Sending request to search for TODO..." -ForegroundColor Yellow
$result = pekobot send $agentName "Use Grep to search for 'TODO|FIXME' in your workspace (case-insensitive). Tell me what you found." --no-stream 2>&1
Write-Host "Response: $result"

if ($result -match "TODO" -or $result -match "FIXME") {
    Write-Host "✓ Found TODO/FIXME comments" -ForegroundColor Green
} else {
    Write-Warning "⚠ Could not verify TODO/FIXME in response"
}

# ============================================================
# TEST 3: Search with regex pattern
# ============================================================
Write-Host "`n========================================" -ForegroundColor Cyan
Write-Host "TEST 3: Search with regex pattern" -ForegroundColor Cyan
Write-Host "========================================" -ForegroundColor Cyan

Write-Host "Sending request to search with regex..." -ForegroundColor Yellow
$result = pekobot send $agentName "Use Grep to search for 'def ' in '*.py' files (Python function definitions). Report the matches." --no-stream 2>&1
Write-Host "Response: $result"

if ($result -match "calculate" -or $result -match "helper") {
    Write-Host "✓ Found Python function definitions" -ForegroundColor Green
} else {
    Write-Warning "⚠ Could not verify Python functions in response"
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
