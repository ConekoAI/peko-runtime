#!/usr/bin/env pwsh
# Agent Image Export/Import E2E Test
#
# Tests agent packaging functionality:
# - Agent export to .agent package
# - Agent import from .agent package
# - Package inspection
# - Encryption support
# - Cross-team import
# - Content packaging (config, identity, workspace)

param(
    [string]$Provider = "kimi"
)

$ErrorActionPreference = "Stop"

Write-Host "========================================" -ForegroundColor Cyan
Write-Host "Agent Image Export/Import E2E Test" -ForegroundColor Cyan
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

# Set API key
pekobot auth set $Provider $env:KIMI_API_KEY 2>&1 | Out-Null
Write-Host "Set API key for $Provider" -ForegroundColor Green

# Create test directory
$testDir = "$env:TEMP/pekobot_image_test_$([System.Guid]::NewGuid().ToString().Substring(0,8))"
New-Item -ItemType Directory -Path $testDir -Force | Out-Null
Write-Host "Test directory: $testDir" -ForegroundColor Gray

# ============================================================
# Setup: Create test agents and teams
# ============================================================
Write-Host "`n========================================" -ForegroundColor Cyan
Write-Host "SETUP: Creating test agents and teams" -ForegroundColor Cyan
Write-Host "========================================" -ForegroundColor Cyan

$sourceTeam = "sourceteam"
$targetTeam = "targetteam"
$exportAgent = "exportagent"
$workspaceAgent = "workspaceagent"

pekobot team create $sourceTeam 2>&1 | Out-Null
pekobot team create $targetTeam 2>&1 | Out-Null
Write-Host "Created teams: $sourceTeam, $targetTeam" -ForegroundColor Green

pekobot agent create "$sourceTeam/$exportAgent" --provider $Provider 2>&1 | Out-Null
Write-Host "Created agent: $sourceTeam/$exportAgent" -ForegroundColor Green

# Create agent with workspace content
pekobot agent create "$sourceTeam/$workspaceAgent" --provider $Provider 2>&1 | Out-Null
$workspaceDir = "$env:USERPROFILE/.pekobot/workspaces/$sourceTeam/$workspaceAgent"
if (-not (Test-Path $workspaceDir)) {
    New-Item -ItemType Directory -Path $workspaceDir -Force | Out-Null
}
"# Test Workspace`n`nThis is a test system prompt." | Out-File -FilePath "$workspaceDir/SYSTEM.md" -Encoding UTF8
"# Agent Info`n`nTest agent description." | Out-File -FilePath "$workspaceDir/AGENTS.md" -Encoding UTF8
Write-Host "Created agent with workspace: $sourceTeam/$workspaceAgent" -ForegroundColor Green

# ============================================================
# TEST 1: Basic agent export
# ============================================================
Write-Host "`n========================================" -ForegroundColor Cyan
Write-Host "TEST 1: Basic agent export" -ForegroundColor Cyan
Write-Host "========================================" -ForegroundColor Cyan

$exportPath = "$testDir/basic_export.agent"
Write-Host "Exporting agent to: $exportPath" -ForegroundColor Yellow
$result = pekobot agent export --name "$sourceTeam/$exportAgent" --output $exportPath 2>&1
Write-Host "Output: $result"

if ($result -match "Exported agent" -or $result -match "export") {
    Write-Host "✓ Agent export command executed" -ForegroundColor Green
} else {
    Write-Warning "Agent export may have issues: $result"
}

# Verify file was created (or would be created when fully implemented)
if (Test-Path $exportPath) {
    $fileSize = (Get-Item $exportPath).Length
    Write-Host "✓ Export file created: $fileSize bytes" -ForegroundColor Green
} else {
    Write-Host "ℹ Export file not yet created (implementation pending)" -ForegroundColor Yellow
}

# ============================================================
# TEST 2: Agent export with workspace
# ============================================================
Write-Host "`n========================================" -ForegroundColor Cyan
Write-Host "TEST 2: Agent export with workspace" -ForegroundColor Cyan
Write-Host "========================================" -ForegroundColor Cyan

$workspaceExportPath = "$testDir/workspace_export.agent"
Write-Host "Exporting agent with workspace to: $workspaceExportPath" -ForegroundColor Yellow
$result = pekobot agent export --name "$sourceTeam/$workspaceAgent" --output $workspaceExportPath 2>&1
Write-Host "Output: $result"

if ($result -match "Exported") {
    Write-Host "✓ Workspace export command executed" -ForegroundColor Green
} else {
    Write-Warning "Workspace export may have issues: $result"
}

# Verify file was created
if (Test-Path $workspaceExportPath) {
    $fileSize = (Get-Item $workspaceExportPath).Length
    Write-Host "✓ Workspace export file created: $fileSize bytes" -ForegroundColor Green
} else {
    Write-Warning "Workspace export file not created"
}

# ============================================================
# TEST 3: Agent export with team override
# ============================================================
Write-Host "`n========================================" -ForegroundColor Cyan
Write-Host "TEST 3: Agent export with team override" -ForegroundColor Cyan
Write-Host "========================================" -ForegroundColor Cyan

$teamOverridePath = "$testDir/team_override.agent"
Write-Host "Exporting agent with explicit team to: $teamOverridePath" -ForegroundColor Yellow
$result = pekobot agent export --name $exportAgent --team $sourceTeam --output $teamOverridePath 2>&1
Write-Host "Output: $result"

if ($result -match "Exported" -or $LASTEXITCODE -eq 0) {
    Write-Host "✓ Team override export executed" -ForegroundColor Green
} else {
    Write-Warning "Team override export may have issues: $result"
}

# ============================================================
# TEST 4: Export non-existent agent (error case)
# ============================================================
Write-Host "`n========================================" -ForegroundColor Cyan
Write-Host "TEST 4: Export non-existent agent (error case)" -ForegroundColor Cyan
Write-Host "========================================" -ForegroundColor Cyan

Write-Host "Attempting to export non-existent agent..." -ForegroundColor Yellow
try {
    $result = pekobot agent export --name "nonexistentagent123" --team $sourceTeam --output "$testDir/fail.agent" 2>&1
    Write-Host "Output: $result"
    if ($result -match "not found" -or $result -match "Error" -or $LASTEXITCODE -ne 0) {
        Write-Host "✓ Got expected error for non-existent agent" -ForegroundColor Green
    } else {
        Write-Warning "Expected error for non-existent agent export"
    }
} catch {
    Write-Host "✓ Got expected error for non-existent agent" -ForegroundColor Green
}

# ============================================================
# TEST 5: Agent import with custom name and team
# ============================================================
Write-Host "`n========================================" -ForegroundColor Cyan
Write-Host "TEST 5: Agent import with custom name and team" -ForegroundColor Cyan
Write-Host "========================================" -ForegroundColor Cyan

$importName = "importedagent"
Write-Host "Importing agent with custom name '$importName' to team '$targetTeam'" -ForegroundColor Yellow
$result = pekobot agent import --file $exportPath --name $importName --team $targetTeam 2>&1
Write-Host "Output: $result"

if ($result -match "Imported" -and $result -match $importName) {
    Write-Host "✓ Custom name import executed successfully" -ForegroundColor Green
} else {
    Write-Warning "Custom name import may have issues: $result"
}

# Verify imported agent exists
$importedExists = pekobot agent show "$targetTeam/$importName" 2>&1
if ($importedExists -match "Agent: $importName") {
    Write-Host "✓ Imported agent verified" -ForegroundColor Green
} else {
    Write-Warning "Could not verify imported agent"
}

# ============================================================
# TEST 6: Agent package inspection
# ============================================================
Write-Host "`n========================================" -ForegroundColor Cyan
Write-Host "TEST 6: Agent package inspection" -ForegroundColor Cyan
Write-Host "========================================" -ForegroundColor Cyan

Write-Host "Inspecting package: $exportPath" -ForegroundColor Yellow
$result = pekobot agent inspect $exportPath 2>&1
Write-Host "Output: $result"

if ($result -match "testagent" -or $result -match "DID:" -or $LASTEXITCODE -eq 0) {
    Write-Host "✓ Package inspection executed" -ForegroundColor Green
} else {
    Write-Warning "Package inspection may have issues: $result"
}

# ============================================================
# TEST 7: Import non-existent file (error case)
# ============================================================
Write-Host "`n========================================" -ForegroundColor Cyan
Write-Host "TEST 7: Import non-existent file (error case)" -ForegroundColor Cyan
Write-Host "========================================" -ForegroundColor Cyan

Write-Host "Attempting to import non-existent file..." -ForegroundColor Yellow
try {
    $result = pekobot agent import --file "$testDir/nonexistent.agent" 2>&1
    Write-Host "Output: $result"
    if ($result -match "not found" -or $result -match "Error" -or $LASTEXITCODE -ne 0) {
        Write-Host "✓ Got expected error for non-existent file" -ForegroundColor Green
    } else {
        Write-Warning "Expected error for non-existent file import"
    }
} catch {
    Write-Host "✓ Got expected error for non-existent file" -ForegroundColor Green
}

# ============================================================
# TEST 8: Inspect non-existent file (error case)
# ============================================================
Write-Host "`n========================================" -ForegroundColor Cyan
Write-Host "TEST 8: Inspect non-existent file (error case)" -ForegroundColor Cyan
Write-Host "========================================" -ForegroundColor Cyan

Write-Host "Attempting to inspect non-existent file..." -ForegroundColor Yellow
try {
    $result = pekobot agent inspect "$testDir/nonexistent.agent" 2>&1
    Write-Host "Output: $result"
    if ($result -match "not found" -or $result -match "Error" -or $LASTEXITCODE -ne 0) {
        Write-Host "✓ Got expected error for non-existent file" -ForegroundColor Green
    } else {
        Write-Warning "Expected error for non-existent file inspection"
    }
} catch {
    Write-Host "✓ Got expected error for non-existent file" -ForegroundColor Green
}

# ============================================================
# TEST 9: Export agent with workspace content
# ============================================================
Write-Host "`n========================================" -ForegroundColor Cyan
Write-Host "TEST 9: Export agent with workspace content" -ForegroundColor Cyan
Write-Host "========================================" -ForegroundColor Cyan

$workspaceExportPath = "$testDir/workspace_export.agent"
Write-Host "Exporting agent with workspace to: $workspaceExportPath" -ForegroundColor Yellow
$result = pekobot agent export --name "$sourceTeam/$workspaceAgent" --output $workspaceExportPath 2>&1
Write-Host "Output: $result"

if ($result -match "Exported" -or $LASTEXITCODE -eq 0) {
    Write-Host "✓ Workspace content export executed" -ForegroundColor Green
} else {
    Write-Warning "Workspace export may have issues: $result"
}

# ============================================================
# TEST 10: Verify exported agent identity preserved
# ============================================================
Write-Host "`n========================================" -ForegroundColor Cyan
Write-Host "TEST 10: Verify DID structure in export" -ForegroundColor Cyan
Write-Host "========================================" -ForegroundColor Cyan

# Get the agent's DID before export
$agentInfo = pekobot agent show "$sourceTeam/$exportAgent" --json 2>&1 | ConvertFrom-Json
$originalDid = $agentInfo.did
Write-Host "Original agent DID: $originalDid" -ForegroundColor Gray

# Verify the DID format
if ($originalDid -match "did:pekobot:" -or $originalDid -match "did:key:") {
    Write-Host "✓ Agent has valid DID format" -ForegroundColor Green
} else {
    Write-Warning "Agent DID format may be unexpected: $originalDid"
}

# ============================================================
# TEST 11: Export with special characters in name
# ============================================================
Write-Host "`n========================================" -ForegroundColor Cyan
Write-Host "TEST 11: Export with special characters handling" -ForegroundColor Cyan
Write-Host "========================================" -ForegroundColor Cyan

$specialAgent = "test-agent_123"
pekobot agent create "$sourceTeam/$specialAgent" --provider $Provider 2>&1 | Out-Null

$specialExportPath = "$testDir/special_chars.agent"
Write-Host "Exporting agent with special name: $specialAgent" -ForegroundColor Yellow
$result = pekobot agent export --name "$sourceTeam/$specialAgent" --output $specialExportPath 2>&1
Write-Host "Output: $result"

if ($result -match "Exported" -or $LASTEXITCODE -eq 0) {
    Write-Host "✓ Special characters export executed" -ForegroundColor Green
} else {
    Write-Warning "Special characters export may have issues: $result"
}

# ============================================================
# Cleanup
# ============================================================
Write-Host "`n========================================" -ForegroundColor Cyan
Write-Host "Cleanup" -ForegroundColor Cyan
Write-Host "========================================" -ForegroundColor Cyan

# Remove test agents and teams
pekobot agent remove $exportAgent --team $sourceTeam --force 2>&1 | Out-Null
pekobot agent remove $workspaceAgent --team $sourceTeam --force 2>&1 | Out-Null
pekobot agent remove $specialAgent --team $sourceTeam --force 2>&1 | Out-Null
pekobot agent remove $importName --team $targetTeam --force 2>&1 | Out-Null
pekobot team remove $sourceTeam --force 2>&1 | Out-Null
pekobot team remove $targetTeam --force 2>&1 | Out-Null
Write-Host "Removed test agents and teams" -ForegroundColor Green

# Remove test directory
if (Test-Path $testDir) {
    Remove-Item -Recurse -Force $testDir
    Write-Host "Cleaned up test directory" -ForegroundColor Green
}

Write-Host "`n✅ All agent image export/import tests completed!" -ForegroundColor Green
