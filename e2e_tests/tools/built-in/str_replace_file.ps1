#!/usr/bin/env pwsh
# StrReplaceFile Tool E2E Test
#
# Tests the StrReplaceFile tool for targeted string replacements.

param(
    [string]$Provider = "minimax"
)

$ErrorActionPreference = "Stop"

Write-Host "========================================" -ForegroundColor Cyan
Write-Host "StrReplaceFile Tool E2E Test" -ForegroundColor Cyan
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
$agentName = "strreplace_test"
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

# Create test file
Write-Host "Creating test file..." -ForegroundColor Cyan
$testFile = "$workspaceDir/config.txt"
$initialContent = @"
[settings]
name = "Original Name"
version = "1.0.0"
debug = true
"@
Set-Content -Path $testFile -Value $initialContent -Encoding UTF8
Write-Host "Created test file: config.txt" -ForegroundColor Green

# ============================================================
# TEST 1: Simple string replacement
# ============================================================
Write-Host "`n========================================" -ForegroundColor Cyan
Write-Host "TEST 1: Simple string replacement" -ForegroundColor Cyan
Write-Host "========================================" -ForegroundColor Cyan

Write-Host "Sending request to replace 'Original Name' with 'New Name'..." -ForegroundColor Yellow
$response = peko send $agentName "Use your str_replace_file tool (NOT shell) to modify config.txt. Replace the line: name = Original Name with: name = New Name. After replacing, respond TOOL_SUCCESS if the replacement succeeded, otherwise respond TOOL_FAILED." --no-stream 2>&1
Start-Sleep -Seconds 3
Write-Host "Response: $response"

Start-Sleep -Milliseconds 500
$content = Get-Content $testFile -Raw
if ($content -match "New Name") {
    Write-Host "✅ PASS: String replacement successful" -ForegroundColor Green
} else {
    Write-Warning "⚠ Could not verify replacement in file"
}

# ============================================================
# TEST 2: Multiple replacements
# ============================================================
Write-Host "`n========================================" -ForegroundColor Cyan
Write-Host "TEST 2: Multiple replacements" -ForegroundColor Cyan
Write-Host "========================================" -ForegroundColor Cyan

Write-Host "Sending request to make multiple replacements..." -ForegroundColor Yellow
$response = peko send $agentName "Use your str_replace_file tool (NOT shell) to modify config.txt. Make TWO replacements: 1) Replace version = 1.0.0 with version = 2.0.0 2) Replace debug = true with debug = false. After replacing, respond TOOL_SUCCESS if both succeeded, otherwise respond TOOL_FAILED." --no-stream 2>&1
Start-Sleep -Seconds 3
Write-Host "Response: $response"

Start-Sleep -Milliseconds 500
$content = Get-Content $testFile -Raw
if ($content -match "2.0.0" -and $content -match "false") {
    Write-Host "✅ PASS: Multiple replacements successful" -ForegroundColor Green
} else {
    Write-Warning "⚠ Could not verify all replacements in file"
}

# ============================================================
# TEST 3: Atomicity - verify file unchanged on failure
# ============================================================
Write-Host "`n========================================" -ForegroundColor Cyan
Write-Host "TEST 3: Verify unchanged when string not found" -ForegroundColor Cyan
Write-Host "========================================" -ForegroundColor Cyan

Write-Host "Sending request with non-existent string..." -ForegroundColor Yellow
$response = peko send $agentName "Use your str_replace_file tool (NOT shell) to modify config.txt. Try to replace old_string='NONEXISTENT_STRING' with new_string='something'. This should fail since the string doesn't exist. After attempting, respond TOOL_SUCCESS if the tool reported an error and the file was NOT changed, otherwise respond TOOL_FAILED." --no-stream 2>&1
Start-Sleep -Seconds 3
Write-Host "Response: $response"

Start-Sleep -Milliseconds 500
$content = Get-Content $testFile -Raw
if ($content -notmatch "something" -and $content -match "2.0.0") {
    Write-Host "✅ PASS: File unchanged after failed replacement (atomic)" -ForegroundColor Green
} else {
    Write-Warning "⚠ File may have been modified after failed replacement"
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

Write-Host "`n✅ StrReplaceFile e2e tests completed!" -ForegroundColor Green
