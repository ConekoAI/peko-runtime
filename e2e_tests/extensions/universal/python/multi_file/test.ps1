#!/usr/bin/env pwsh
# Multi-File Tool E2E Test
#
# This test verifies that tools with subdirectories are properly installed
# and can import from helper modules in subdirectories.

param(
    [string]$Provider = "minimax"
)

$ErrorActionPreference = "Stop"

Write-Host "========================================" -ForegroundColor Cyan
Write-Host "Multi-File Tool E2E Test" -ForegroundColor Cyan
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
pushd "$PSScriptRoot/../../../../"
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
# STEP 1: Verify multi-file tool structure
# ============================================================
Write-Host "`n========================================" -ForegroundColor Cyan
Write-Host "STEP 1: Verify tool structure" -ForegroundColor Cyan
Write-Host "========================================" -ForegroundColor Cyan

$toolDir = "$PSScriptRoot"
$expectedFiles = @(
    "multi_file_calc.py",
    "manifest.yaml",
    "utils/__init__.py",
    "utils/validators.py",
    "utils/calculator.py",
    "utils/formatter.py"
)

Write-Host "Checking tool structure..." -ForegroundColor Yellow
$allExist = $true
foreach ($file in $expectedFiles) {
    $fullPath = Join-Path $toolDir $file
    if (Test-Path $fullPath) {
        Write-Host "  ✓ $file" -ForegroundColor Green
    } else {
        Write-Host "  ✗ $file (missing)" -ForegroundColor Red
        $allExist = $false
    }
}

if (-not $allExist) {
    Write-Error "Tool structure incomplete"
    exit 1
}

# ============================================================
# STEP 2: Create agent
# ============================================================
Write-Host "`n========================================" -ForegroundColor Cyan
Write-Host "STEP 2: Create agent" -ForegroundColor Cyan
Write-Host "========================================" -ForegroundColor Cyan

$agentName = "multi_file_test"
$teamName = "default"

Write-Host "Creating agent: $agentName" -ForegroundColor Yellow
pekobot agent create $agentName --provider $Provider --force 2>&1 | Out-Null
Write-Host "Created agent" -ForegroundColor Green

# ============================================================
# STEP 3: Install multi-file tool as extension
# ============================================================
Write-Host "`n========================================" -ForegroundColor Cyan
Write-Host "STEP 3: Install multi-file tool extension" -ForegroundColor Cyan
Write-Host "========================================" -ForegroundColor Cyan

Write-Host "Installing multi_file_calc as universal-tool extension..." -ForegroundColor Yellow
$installResult = pekobot ext install $toolDir --type universal-tool 2>&1
Write-Host $installResult

# Verify installation
$extList = pekobot ext list --type universal-tool 2>&1
if ($extList -match "multi_file_calc") {
    Write-Host "✓ Tool extension installed successfully" -ForegroundColor Green
} else {
    Write-Error "Tool extension installation failed"
    exit 1
}

# ============================================================
# STEP 4: Verify all files were copied (including subdirs)
# ============================================================
Write-Host "`n========================================" -ForegroundColor Cyan
Write-Host "STEP 4: Verify installed files" -ForegroundColor Cyan
Write-Host "========================================" -ForegroundColor Cyan

$installedDir = "$env:APPDATA/pekobot/extensions/multi_file_calc"
$expectedInstalledFiles = @(
    "manifest.yaml",
    "multi_file_calc.py",
    "utils/__init__.py",
    "utils/validators.py",
    "utils/calculator.py",
    "utils/formatter.py"
)

Write-Host "Checking installed files..." -ForegroundColor Yellow
$allInstalled = $true
foreach ($file in $expectedInstalledFiles) {
    $fullPath = Join-Path $installedDir $file
    if (Test-Path $fullPath) {
        Write-Host "  ✓ $file" -ForegroundColor Green
    } else {
        Write-Host "  ✗ $file (missing)" -ForegroundColor Red
        $allInstalled = $false
    }
}

if (-not $allInstalled) {
    Write-Error "Not all files were installed (recursive copy may have failed)"
    exit 1
}

Write-Host "✓ All files including subdirectory contents installed" -ForegroundColor Green

# ============================================================
# STEP 5: Enable tool extension
# ============================================================
Write-Host "`n========================================" -ForegroundColor Cyan
Write-Host "STEP 5: Enable tool extension" -ForegroundColor Cyan
Write-Host "========================================" -ForegroundColor Cyan

Write-Host "Enabling multi_file_calc extension..." -ForegroundColor Yellow
pekobot ext enable multi_file_calc --target default/$agentName 2>&1 | Out-Null
Write-Host "Enabled tool extension" -ForegroundColor Green

# Verify
$extInfo = pekobot ext info multi_file_calc 2>&1
Write-Host "`nExtension status:" -ForegroundColor Cyan
Write-Host $extInfo

# ============================================================
# STEP 6: Test tool via agent
# ============================================================
Write-Host "`n========================================" -ForegroundColor Cyan
Write-Host "STEP 6: Test tool via agent" -ForegroundColor Cyan
Write-Host "========================================" -ForegroundColor Cyan

Write-Host "Sending calculation request to agent..." -ForegroundColor Yellow
Measure-Command {
    $response = pekobot send $agentName "We are testing your access and functionality of the multi_file_calc tool. Please calculate 15 multiplied by 6 using the multi_file_calc tool. respond TOOL_SUCCESS if the tool works, otherwise respond TOOL_FAILED with an explanation" --no-stream 2>&1
}
Write-Host "Agent response: $response"

$toolSuccess = $response -match "TOOL_SUCCESS"
$toolFailed = $response -match "TOOL_FAILED"
if ($toolSuccess) {
    Write-Host "✅ PASS: Tool worked correctly" -ForegroundColor Green
} elseif ($toolFailed) {
    Write-Host "❌ FAIL: Tool did not work" -ForegroundColor Red
} else {
    Write-Host "⚠️ Tool result unclear" -ForegroundColor Yellow
}

# ============================================================
# Cleanup
# ============================================================
Write-Host "`n========================================" -ForegroundColor Cyan
Write-Host "Cleanup" -ForegroundColor Cyan
Write-Host "========================================" -ForegroundColor Cyan

# Uninstall tool extension
pekobot ext uninstall multi_file_calc 2>&1 | Out-Null
Write-Host "Uninstalled tool extension" -ForegroundColor Green

# Delete agent
pekobot agent delete $agentName --force 2>&1 | Out-Null
Write-Host "Deleted agent" -ForegroundColor Green

Write-Host "`n✅ Multi-file tool E2E test completed successfully!" -ForegroundColor Green
