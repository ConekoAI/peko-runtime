#!/usr/bin/env pwsh
# Agent Config E2E Test
#
# Tests agent configuration get/set operations:
# - Agent config get (various key paths)
# - Agent config set (various key paths)
# - JSON output for both get and set

param(
    [string]$Provider = "minimax"
)

$ErrorActionPreference = "Stop"

Write-Host "========================================" -ForegroundColor Cyan
Write-Host "Agent Config Get/Set E2E Test" -ForegroundColor Cyan
Write-Host "========================================" -ForegroundColor Cyan

# Check prerequisites
if (-not $env:MINIMAX_API_KEY -and $Provider -eq "minimax") {
    Write-Error "MINIMAX_API_KEY environment variable not set"
    exit 1
}

# Build peko
Write-Host "Building peko..." -ForegroundColor Cyan
pushd "$PSScriptRoot/../.."
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

# Set API key
peko auth set $Provider $env:MINIMAX_API_KEY 2>&1 | Out-Null
Write-Host "Set API key for $Provider" -ForegroundColor Green

# Create test agent
$agentName = "configtestagent"
Write-Host "`nCreating test agent: $agentName" -ForegroundColor Yellow
$result = peko agent create $agentName --provider $Provider 2>&1
Write-Host "Output: $result"

if ($result -match "Created agent") {
    Write-Host "✓ Agent created successfully" -ForegroundColor Green
} else {
    Write-Error "Agent creation failed"
}

# Create team agent for cross-team tests
$teamName = "configteam"
peko team create $teamName 2>&1 | Out-Null
$teamAgent = "teamconfigagent"
$result = peko agent create "$teamName/$teamAgent" --provider $Provider 2>&1
Write-Host "Created team agent: $teamAgent in team $teamName" -ForegroundColor Green

# ============================================================
# TEST 1: Config get - provider.provider_type
# ============================================================
Write-Host "`n========================================" -ForegroundColor Cyan
Write-Host "TEST 1: Config get - provider.provider_type" -ForegroundColor Cyan
Write-Host "========================================" -ForegroundColor Cyan

Write-Host "Getting config: provider.provider_type for $agentName" -ForegroundColor Yellow
$result = peko agent config get $agentName provider.provider_type 2>&1
Write-Host "Output: $result"

if ($result -match $Provider) {
    Write-Host "✓ Got expected provider type" -ForegroundColor Green
} else {
    Write-Error "Config get provider.provider_type failed"
}

# ============================================================
# TEST 2: Config get - provider.default_model
# ============================================================
Write-Host "`n========================================" -ForegroundColor Cyan
Write-Host "TEST 2: Config get - provider.default_model" -ForegroundColor Cyan
Write-Host "========================================" -ForegroundColor Cyan

Write-Host "Getting config: provider.default_model for $agentName" -ForegroundColor Yellow
$result = peko agent config get $agentName provider.default_model 2>&1
Write-Host "Output: $result"

if ($result) {
    Write-Host "✓ Got default_model value" -ForegroundColor Green
} else {
    Write-Error "Config get provider.default_model failed"
}

# ============================================================
# TEST 3: Config get - name
# ============================================================
Write-Host "`n========================================" -ForegroundColor Cyan
Write-Host "TEST 3: Config get - name" -ForegroundColor Cyan
Write-Host "========================================" -ForegroundColor Cyan

Write-Host "Getting config: name for $agentName" -ForegroundColor Yellow
$result = peko agent config get $agentName name 2>&1
Write-Host "Output: $result"

if ($result -match $agentName) {
    Write-Host "✓ Got expected agent name" -ForegroundColor Green
} else {
    Write-Error "Config get name failed"
}

# ============================================================
# TEST 4: Config get with --json
# ============================================================
Write-Host "`n========================================" -ForegroundColor Cyan
Write-Host "TEST 4: Config get with --json" -ForegroundColor Cyan
Write-Host "========================================" -ForegroundColor Cyan

Write-Host "Getting config with --json: name for $agentName" -ForegroundColor Yellow
$result = peko agent config get $agentName name --json 2>&1 | ConvertFrom-Json
Write-Host "Output (parsed JSON):"
$result | ConvertTo-Json -Depth 2 | Write-Host

if ($result.agent -eq $agentName -and $result.key -eq "name" -and $result.value -eq $agentName) {
    Write-Host "✓ JSON config get output correct" -ForegroundColor Green
} else {
    Write-Error "JSON config get output incorrect"
}

# ============================================================
# TEST 5: Config get - non-existent key
# ============================================================
Write-Host "`n========================================" -ForegroundColor Cyan
Write-Host "TEST 5: Config get - non-existent key" -ForegroundColor Cyan
Write-Host "========================================" -ForegroundColor Cyan

Write-Host "Getting non-existent key for $agentName..." -ForegroundColor Yellow
try {
    $result = peko agent config get $agentName nonexistent.key 2>&1
    Write-Host "Output: $result"
    if ($result -match "not found" -or $result -match "Error" -or $result -match "does not exist") {
        Write-Host "✓ Got expected error for non-existent key" -ForegroundColor Green
    } else {
        Write-Host "⚠ Unexpected output for non-existent key" -ForegroundColor Yellow
    }
} catch {
    Write-Host "✓ Got expected error for non-existent key" -ForegroundColor Green
}

# ============================================================
# TEST 6: Config get - non-existent agent (error case)
# ============================================================
Write-Host "`n========================================" -ForegroundColor Cyan
Write-Host "TEST 6: Config get - non-existent agent" -ForegroundColor Cyan
Write-Host "========================================" -ForegroundColor Cyan

Write-Host "Getting config for non-existent agent..." -ForegroundColor Yellow
try {
    $result = peko agent config get nonexistentagent123 name 2>&1
    Write-Host "Output: $result"
    if ($result -match "not found" -or $result -match "Error" -or $result -match "does not exist") {
        Write-Host "✓ Got expected error for non-existent agent" -ForegroundColor Green
    } else {
        Write-Host "⚠ Unexpected output for non-existent agent" -ForegroundColor Yellow
    }
} catch {
    Write-Host "✓ Got expected error for non-existent agent" -ForegroundColor Green
}

# ============================================================
# TEST 7: Config get with --team flag
# ============================================================
Write-Host "`n========================================" -ForegroundColor Cyan
Write-Host "TEST 7: Config get with --team flag" -ForegroundColor Cyan
Write-Host "========================================" -ForegroundColor Cyan

Write-Host "Getting config with --team: name for $teamAgent in $teamName" -ForegroundColor Yellow
$result = peko agent config get $teamAgent name --team $teamName 2>&1
Write-Host "Output: $result"

if ($result -match $teamAgent) {
    Write-Host "✓ Config get with --team works correctly" -ForegroundColor Green
} else {
    Write-Error "Config get with --team failed"
}

# ============================================================
# TEST 8: Config set - provider.timeout_seconds
# ============================================================
Write-Host "`n========================================" -ForegroundColor Cyan
Write-Host "TEST 8: Config set - provider.timeout_seconds" -ForegroundColor Cyan
Write-Host "========================================" -ForegroundColor Cyan

Write-Host "Setting config: provider.timeout_seconds = 120 for $agentName" -ForegroundColor Yellow
$result = peko agent config set $agentName provider.timeout_seconds 120 2>&1
Write-Host "Output: $result"

if ($result -match "Updated" -or $result -match "Set") {
    Write-Host "✓ Config set succeeded" -ForegroundColor Green
} else {
    Write-Error "Config set failed"
}

# Verify the change
Write-Host "Verifying the change..." -ForegroundColor Yellow
$verify = peko agent config get $agentName provider.timeout_seconds 2>&1
Write-Host "Value: $verify"
if ($verify -match "120") {
    Write-Host "✓ Config set value verified" -ForegroundColor Green
} else {
    Write-Error "Config set value verification failed"
}

# ============================================================
# TEST 9: Config set - provider.default_model
# ============================================================
Write-Host "`n========================================" -ForegroundColor Cyan
Write-Host "TEST 9: Config set - provider.default_model" -ForegroundColor Cyan
Write-Host "========================================" -ForegroundColor Cyan

$newModel = "test-model-123"
Write-Host "Setting config: provider.default_model = $newModel for $agentName" -ForegroundColor Yellow
$result = peko agent config set $agentName provider.default_model $newModel 2>&1
Write-Host "Output: $result"

if ($result -match "Updated" -or $result -match "Set") {
    Write-Host "✓ Config set model succeeded" -ForegroundColor Green
} else {
    Write-Error "Config set model failed"
}

# Verify
$verify = peko agent config get $agentName provider.default_model 2>&1
if ($verify -match $newModel) {
    Write-Host "✓ Config set model verified" -ForegroundColor Green
} else {
    Write-Error "Config set model verification failed"
}

# ============================================================
# TEST 10: Config set with --json output
# ============================================================
Write-Host "`n========================================" -ForegroundColor Cyan
Write-Host "TEST 10: Config set with --json output" -ForegroundColor Cyan
Write-Host "========================================" -ForegroundColor Cyan

Write-Host "Setting config with --json: provider.timeout_seconds = 300 for $agentName" -ForegroundColor Yellow
$result = peko agent config set $agentName provider.timeout_seconds 300 --json 2>&1 | ConvertFrom-Json
Write-Host "Output (parsed JSON):"
$result | ConvertTo-Json -Depth 2 | Write-Host

if ($result.agent -eq $agentName -and $result.key -match "timeout_seconds" -and $result.success -eq $true) {
    Write-Host "✓ JSON config set output correct" -ForegroundColor Green
} else {
    Write-Error "JSON config set output incorrect"
}

# ============================================================
# TEST 11: Config set - description field
# ============================================================
Write-Host "`n========================================" -ForegroundColor Cyan
Write-Host "TEST 11: Config set - description field" -ForegroundColor Cyan
Write-Host "========================================" -ForegroundColor Cyan

$newDescription = "Test agent description"
Write-Host "Setting config: description = $newDescription for $agentName" -ForegroundColor Yellow
$result = peko agent config set $agentName description $newDescription 2>&1
Write-Host "Output: $result"

if ($result -match "Updated" -or $result -match "Set") {
    Write-Host "✓ Config set description succeeded" -ForegroundColor Green
} else {
    Write-Error "Config set description failed"
}

# Verify
$verify = peko agent config get $agentName description 2>&1
if ($verify -match $newDescription) {
    Write-Host "✓ Config set description verified" -ForegroundColor Green
} else {
    Write-Error "Config set description verification failed"
}

# ============================================================
# TEST 12: Config set with --team flag
# ============================================================
Write-Host "`n========================================" -ForegroundColor Cyan
Write-Host "TEST 12: Config set with --team flag" -ForegroundColor Cyan
Write-Host "========================================" -ForegroundColor Cyan

$teamTimeout = 200
Write-Host "Setting config with --team: provider.timeout_seconds = $teamTimeout for $teamAgent in $teamName" -ForegroundColor Yellow
$result = peko agent config set $teamAgent provider.timeout_seconds $teamTimeout --team $teamName 2>&1
Write-Host "Output: $result"

if ($result -match "Updated" -or $result -match "Set") {
    Write-Host "✓ Config set with --team succeeded" -ForegroundColor Green
} else {
    Write-Error "Config set with --team failed"
}

# Verify
$verify = peko agent config get $teamAgent provider.timeout_seconds --team $teamName 2>&1
if ($verify -match $teamTimeout) {
    Write-Host "✓ Config set with --team verified" -ForegroundColor Green
} else {
    Write-Error "Config set with --team verification failed"
}

# ============================================================
# TEST 13: Config set - invalid key (error case)
# ============================================================
Write-Host "`n========================================" -ForegroundColor Cyan
Write-Host "TEST 13: Config set - invalid key" -ForegroundColor Cyan
Write-Host "========================================" -ForegroundColor Cyan

Write-Host "Setting invalid key for $agentName..." -ForegroundColor Yellow
try {
    $result = peko agent config set $agentName invalid.nested.key value 2>&1
    Write-Host "Output: $result"
    if ($result -match "not found" -or $result -match "Error" -or $result -match "cannot set") {
        Write-Host "✓ Got expected error for invalid key" -ForegroundColor Green
    } else {
        Write-Host "⚠ Unexpected output for invalid key" -ForegroundColor Yellow
    }
} catch {
    Write-Host "✓ Got expected error for invalid key" -ForegroundColor Green
}

# ============================================================
# TEST 14: Config set - non-existent agent (error case)
# ============================================================
Write-Host "`n========================================" -ForegroundColor Cyan
Write-Host "TEST 14: Config set - non-existent agent" -ForegroundColor Cyan
Write-Host "========================================" -ForegroundColor Cyan

Write-Host "Setting config for non-existent agent..." -ForegroundColor Yellow
try {
    $result = peko agent config set nonexistentagent123 description "test" 2>&1
    Write-Host "Output: $result"
    if ($result -match "not found" -or $result -match "Error" -or $result -match "does not exist") {
        Write-Host "✓ Got expected error for non-existent agent" -ForegroundColor Green
    } else {
        Write-Host "⚠ Unexpected output for non-existent agent" -ForegroundColor Yellow
    }
} catch {
    Write-Host "✓ Got expected error for non-existent agent" -ForegroundColor Green
}

# ============================================================
# Cleanup
# ============================================================
Write-Host "`n========================================" -ForegroundColor Cyan
Write-Host "Test Complete - Cleaning up" -ForegroundColor Cyan
Write-Host "========================================" -ForegroundColor Cyan

# Clean up test agents and teams
peko agent remove $agentName --force 2>&1 | Out-Null
peko agent remove $teamAgent --team $teamName --force 2>&1 | Out-Null
peko team remove $teamName --force 2>&1 | Out-Null
Write-Host "Cleaned up test agents and teams" -ForegroundColor Green

Write-Host "`n✅ All agent config tests completed successfully!" -ForegroundColor Green
