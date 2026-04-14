#!/usr/bin/env pwsh
# WriteFile Tool E2E Test
#
# Tests the WriteFile tool for creating and writing files.

param(
    [string]$Provider = "kimi"
)

$ErrorActionPreference = "Stop"

Write-Host "========================================" -ForegroundColor Cyan
Write-Host "WriteFile Tool E2E Test" -ForegroundColor Cyan
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

# Create agent
$agentName = "writefile_test"
pekobot agent create $agentName --provider $Provider 2>&1 | Out-Null
Write-Host "Created agent: $agentName" -ForegroundColor Green

# Enable granular tools via extension framework
pekobot ext enable read_file --target default/$agentName 2>&1 | Out-Null
pekobot ext enable write_file --target default/$agentName 2>&1 | Out-Null
pekobot ext enable glob --target default/$agentName 2>&1 | Out-Null
pekobot ext enable grep --target default/$agentName 2>&1 | Out-Null
pekobot ext enable str_replace_file --target default/$agentName 2>&1 | Out-Null
Write-Host "Enabled granular filesystem tools via extension framework" -ForegroundColor Green

# Get workspace directory
$workspaceDir = "$env:APPDATA/pekobot/workspaces/default/$agentName"

# ============================================================
# TEST 1: Create a new file with WriteFile
# ============================================================
Write-Host "`n========================================" -ForegroundColor Cyan
Write-Host "TEST 1: Create a new file" -ForegroundColor Cyan
Write-Host "========================================" -ForegroundColor Cyan

Write-Host "Sending request to create a file..." -ForegroundColor Yellow
$response = pekobot send $agentName "Use your write_file tool (NOT shell) to create a file called 'hello.txt' in your workspace with the content 'Hello from write_file tool!'. Use mode='create'. After writing, respond TOOL_SUCCESS if the file was created, otherwise respond TOOL_FAILED." --no-stream 2>&1
Write-Host "Response: $response"

$testFile = "$workspaceDir/hello.txt"
if (Test-Path $testFile) {
    $content = Get-Content $testFile -Raw
    if ($content -match "Hello from write_file tool") {
        Write-Host "✅ PASS: File created with correct content" -ForegroundColor Green
    } else {
        Write-Warning "⚠ File created but content doesn't match"
    }
} else {
    Write-Warning "⚠ File not found after write"
}

# ============================================================
# TEST 2: Overwrite existing file
# ============================================================
Write-Host "`n========================================" -ForegroundColor Cyan
Write-Host "TEST 2: Overwrite existing file" -ForegroundColor Cyan
Write-Host "========================================" -ForegroundColor Cyan

Write-Host "Sending request to overwrite the file..." -ForegroundColor Yellow
$response = pekobot send $agentName "Use your write_file tool (NOT shell) to overwrite 'hello.txt' with the content 'Updated content!' using mode='overwrite'. After writing, respond TOOL_SUCCESS if the file was updated, otherwise respond TOOL_FAILED." --no-stream 2>&1
Write-Host "Response: $response"

Start-Sleep -Milliseconds 500
if (Test-Path $testFile) {
    $content = Get-Content $testFile -Raw
    if ($content -match "Updated content") {
        Write-Host "✅ PASS: File overwritten successfully" -ForegroundColor Green
    } else {
        Write-Warning "⚠ File content doesn't match expected"
    }
} else {
    Write-Warning "⚠ File not found after overwrite"
}

# ============================================================
# TEST 3: Create nested directory file
# ============================================================
Write-Host "`n========================================" -ForegroundColor Cyan
Write-Host "TEST 3: Create file in nested directory" -ForegroundColor Cyan
Write-Host "========================================" -ForegroundColor Cyan

Write-Host "Sending request to create nested file..." -ForegroundColor Yellow
$response = pekobot send $agentName "Use your write_file tool (NOT shell) to create 'subdir/nested.txt' with content 'nested file content' in your workspace. Create any needed directories with mode='create'. After writing, respond TOOL_SUCCESS if the file was created, otherwise respond TOOL_FAILED." --no-stream 2>&1
Write-Host "Response: $response"

$nestedFile = "$workspaceDir/subdir/nested.txt"
Start-Sleep -Milliseconds 500
if (Test-Path $nestedFile) {
    $content = Get-Content $nestedFile -Raw
    if ($content -match "nested file content") {
        Write-Host "✅ PASS: Nested file created successfully" -ForegroundColor Green
    } else {
        Write-Warning "⚠ Nested file content doesn't match"
    }
} else {
    Write-Warning "⚠ Nested file not found"
}

# ============================================================
# Cleanup
# ============================================================
Write-Host "`n========================================" -ForegroundColor Cyan
Write-Host "Test Complete - Cleaning up" -ForegroundColor Cyan
Write-Host "========================================" -ForegroundColor Cyan

# Clean up test files
if (Test-Path "$workspaceDir/hello.txt") {
    Remove-Item "$workspaceDir/hello.txt" -Force
}
if (Test-Path "$workspaceDir/subdir") {
    Remove-Item "$workspaceDir/subdir" -Recurse -Force
}
Write-Host "Removed test files" -ForegroundColor Green

pekobot agent delete $agentName --force 2>&1 | Out-Null
Write-Host "Deleted test agent" -ForegroundColor Green

Write-Host "`n✅ WriteFile e2e tests completed!" -ForegroundColor Green
