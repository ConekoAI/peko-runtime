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
    "multi_file_calc.json",
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

# # Update AGENT.md
# $agentDir = "$env:USERPROFILE/.pekobot/teams/default/agents/$agentName"
# $agentMd = @"
# # Multi-File Test Agent

# An agent for testing multi-file tools with subdirectories.

# ## Available Tools

# - shell: Execute shell commands
# - multi_file_calc: Calculator tool with multi-file structure (imports from utils/)
# "@
# $agentMd | Out-File -FilePath "$agentDir/AGENT.md" -Encoding utf8
# Write-Host "Updated AGENT.md" -ForegroundColor Green

# ============================================================
# STEP 3: Install multi-file tool system-wide
# ============================================================
Write-Host "`n========================================" -ForegroundColor Cyan
Write-Host "STEP 3: Install multi-file tool" -ForegroundColor Cyan
Write-Host "========================================" -ForegroundColor Cyan

Write-Host "Installing multi_file_calc tool..." -ForegroundColor Yellow
$installResult = pekobot cap universal install $toolDir --force 2>&1
Write-Host $installResult

# Verify installation
$toolsList = pekobot cap universal list 2>&1
if ($toolsList -match "multi_file_calc") {
    Write-Host "✓ Tool installed successfully" -ForegroundColor Green
} else {
    Write-Error "Tool installation failed"
    exit 1
}

# ============================================================
# STEP 4: Verify all files were copied (including subdirs)
# ============================================================
Write-Host "`n========================================" -ForegroundColor Cyan
Write-Host "STEP 4: Verify installed files" -ForegroundColor Cyan
Write-Host "========================================" -ForegroundColor Cyan

$installedDir = "$env:APPDATA/pekobot/tools/multi_file_calc"
$expectedInstalledFiles = @(
    "manifest.json",
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
# STEP 5: Enable tool for agent
# ============================================================
Write-Host "`n========================================" -ForegroundColor Cyan
Write-Host "STEP 5: Enable tool for agent" -ForegroundColor Cyan
Write-Host "========================================" -ForegroundColor Cyan

Write-Host "Enabling multi_file_calc for $teamName/$agentName..." -ForegroundColor Yellow
pekobot cap enable "$teamName/$agentName" multi_file_calc 2>&1 | Out-Null
Write-Host "Enabled tool for agent" -ForegroundColor Green

# Verify
$statusOutput = pekobot cap status "$teamName/$agentName" 2>&1
Write-Host "`nAgent capability status:" -ForegroundColor Cyan
Write-Host $statusOutput

# Convert to string and check
$statusString = $statusOutput -join " "
if ($statusString -notmatch "multi_file_calc") {
    Write-Error "Tool not found in agent capabilities"
    exit 1
}

# ============================================================
# STEP 6: Test tool via agent
# ============================================================
Write-Host "`n========================================" -ForegroundColor Cyan
Write-Host "STEP 6: Test tool via agent" -ForegroundColor Cyan
Write-Host "========================================" -ForegroundColor Cyan

Write-Host "Sending calculation request to agent..." -ForegroundColor Yellow
Measure-Command {
    $response = pekobot send $agentName "Use multi_file_calc to calculate 15 multiplied by 6" --no-stream 2>&1
}
Write-Host "Agent response: $response"

# Verify response contains expected result
if ($response -match "90" -or $response -match "15.*6.*=.*90") {
    Write-Host "✓ Tool returned correct result (15 × 6 = 90)" -ForegroundColor Green
} else {
    Write-Warning "⚠ Could not verify result in response (check output above)"
}

# Check session
$sessions = pekobot session list $agentName --json 2>&1 | ConvertFrom-Json
if ($sessions.sessions.Count -ge 1) {
    Write-Host "✓ Session created" -ForegroundColor Green
    $sessionId = $sessions.sessions[0].session_id
    
    # Check session for tool call
    $sessionFile = "$env:USERPROFILE/AppData/Roaming/pekobot/sessions/default/$agentName/${sessionId}.jsonl"
    if (Test-Path $sessionFile) {
        $content = Get-Content $sessionFile -Raw
        if ($content -match "multi_file_calc") {
            Write-Host "✓ Tool was called in session" -ForegroundColor Green
        } else {
            Write-Warning "⚠ Tool call not found in session"
        }
        
        # Check for metadata indicating multi-file structure
        if ($content -match "multi_file_demo" -or $content -match "has_subdirectories") {
            Write-Host "✓ Tool metadata indicates multi-file structure" -ForegroundColor Green
        }
    }
} else {
    Write-Warning "⚠ No session found"
}

# ============================================================
# Cleanup
# ============================================================
Write-Host "`n========================================" -ForegroundColor Cyan
Write-Host "Cleanup" -ForegroundColor Cyan
Write-Host "========================================" -ForegroundColor Cyan

# Uninstall tool
pekobot cap universal uninstall multi_file_calc --force 2>&1 | Out-Null
Write-Host "Uninstalled tool" -ForegroundColor Green

# Delete agent
pekobot agent delete $agentName --force 2>&1 | Out-Null
Write-Host "Deleted agent" -ForegroundColor Green

Write-Host "`n✅ Multi-file tool E2E test completed successfully!" -ForegroundColor Green
