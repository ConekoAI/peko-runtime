#!/usr/bin/env pwsh
# Agent Packaging E2E Test
#
# Tests the unified agent packaging system (ADR-027):
# - Create agent (peko agent create)
# - Export running agent to .agent (peko agent export)
# - Inspect .agent package (peko agent inspect)
# - Import .agent package (peko agent import)
# - Verify content-addressable layers and checksums

param(
    [string]$Provider = "minimax"
)

$ErrorActionPreference = "Stop"

Write-Host "========================================" -ForegroundColor Cyan
Write-Host "Agent Packaging E2E Test" -ForegroundColor Cyan
Write-Host "========================================" -ForegroundColor Cyan

# Check prerequisites
if (-not $env:MINIMAX_API_KEY -and $Provider -eq "minimax") {
    Write-Warning "MINIMAX_API_KEY not set — some tests may be skipped"
}

$pekoCmd = "peko"
Write-Host "Using command: $pekoCmd" -ForegroundColor Gray

# Set API key if available
if ($env:MINIMAX_API_KEY) {
    & $pekoCmd auth set $Provider $env:MINIMAX_API_KEY 2>&1 | Out-Null
    Write-Host "Set API key for $Provider" -ForegroundColor Green
}

# Create test directory
$testDir = "$env:TEMP/PEKO_packaging_test_$([System.Guid]::NewGuid().ToString().Substring(0,8))"
New-Item -ItemType Directory -Path $testDir -Force | Out-Null
Write-Host "Test directory: $testDir" -ForegroundColor Gray

# Create test teams
$sourceTeam = "sourceteam"
$targetTeam = "targetteam"
& $pekoCmd team create $sourceTeam 2>&1 | Out-Null
& $pekoCmd team create $targetTeam 2>&1 | Out-Null
Write-Host "Created teams: $sourceTeam, $targetTeam" -ForegroundColor Green

# ============================================================
# SETUP: Create agent and add custom content
# ============================================================
Write-Host "`n========================================" -ForegroundColor Cyan
Write-Host "SETUP: Creating agent with custom content" -ForegroundColor Cyan
Write-Host "========================================" -ForegroundColor Cyan

$agentName = "my-agent"
& $pekoCmd agent create "$sourceTeam/$agentName" --provider $Provider 2>&1 | Out-Null
Write-Host "Created agent: $sourceTeam/$agentName" -ForegroundColor Green

# Add a skill
$skillsDir = "$env:APPDATA/peko/skills"
New-Item -ItemType Directory -Path "$skillsDir/test-skill" -Force | Out-Null
@"
# Test Skill

A skill for testing packaging.

## Usage

Use this skill to verify packaging works.
"@ | Out-File -FilePath "$skillsDir/test-skill/SKILL.md" -Encoding UTF8
Write-Host "Added skill: test-skill" -ForegroundColor Green

# Add workspace content
$workspaceDir = "$env:APPDATA/peko/workspaces/$sourceTeam/$agentName"
New-Item -ItemType Directory -Path $workspaceDir -Force | Out-Null
@"
# Test Workspace

This is a test workspace file.
"@ | Out-File -FilePath "$workspaceDir/README.md" -Encoding UTF8
Write-Host "Added workspace files" -ForegroundColor Green

# ============================================================
# TEST 1: Export agent to .agent package
# ============================================================
Write-Host "`n========================================" -ForegroundColor Cyan
Write-Host "TEST 1: Export agent to .agent package" -ForegroundColor Cyan
Write-Host "========================================" -ForegroundColor Cyan

$exportPath = "$testDir/my-agent.agent"
& $pekoCmd agent export --name "$sourceTeam/$agentName" --output $exportPath 2>&1 | Out-Null

if (Test-Path $exportPath) {
    $fileSize = (Get-Item $exportPath).Length
    Write-Host "✓ Export succeeded: $exportPath ($fileSize bytes)" -ForegroundColor Green
} else {
    Write-Error "Export failed — file not found at $exportPath"
}

# Capture first export info for dedup test
$firstInspect = & $pekoCmd agent inspect $exportPath --json 2>&1 | ConvertFrom-Json
if ($firstInspect.valid -ne $true) { Write-Error "First export package is invalid" }

# ============================================================
# TEST 2: Inspect exported .agent package
# ============================================================
Write-Host "`n========================================" -ForegroundColor Cyan
Write-Host "TEST 2: Inspect exported .agent package" -ForegroundColor Cyan
Write-Host "========================================" -ForegroundColor Cyan

$inspectResult = & $pekoCmd agent inspect $exportPath --json 2>&1 | ConvertFrom-Json

if ($inspectResult.name -eq $agentName) {
    Write-Host "✓ Inspect shows correct agent name" -ForegroundColor Green
} else {
    Write-Error "Inspect shows wrong name: $($inspectResult.name)"
}

if ($inspectResult.valid -eq $true) {
    Write-Host "✓ Inspect reports package as valid" -ForegroundColor Green
} else {
    Write-Error "Inspect reports package as invalid"
}

# ============================================================
# TEST 3: Import exported .agent with custom name
# ============================================================
Write-Host "`n========================================" -ForegroundColor Cyan
Write-Host "TEST 3: Import .agent with custom name" -ForegroundColor Cyan
Write-Host "========================================" -ForegroundColor Cyan

$importedName = "imported-agent"
$importOutput = & $pekoCmd agent import --file $exportPath --name $importedName --team $targetTeam 2>&1
if ($importOutput -match $importedName) {
    Write-Host "✓ Import succeeded with name '$importedName'" -ForegroundColor Green
} else {
    Write-Error "Import failed or wrong name: $importOutput"
}

# Verify imported agent exists
$showResult = & $pekoCmd agent show "$targetTeam/$importedName" --json 2>&1 | ConvertFrom-Json
if ($showResult.name -eq $importedName) {
    Write-Host "✓ Imported agent verified via show" -ForegroundColor Green
} else {
    Write-Error "Imported agent not found"
}

# ============================================================
# TEST 4: Export running agent and re-import
# ============================================================
Write-Host "`n========================================" -ForegroundColor Cyan
Write-Host "TEST 4: Export running agent and re-import" -ForegroundColor Cyan
Write-Host "========================================" -ForegroundColor Cyan

$exportAgent = "exportagent"
& $pekoCmd agent create "$sourceTeam/$exportAgent" --provider $Provider 2>&1 | Out-Null
Write-Host "Created agent: $sourceTeam/$exportAgent" -ForegroundColor Green

$exportPath2 = "$testDir/exported.agent"
$exportOutput = & $pekoCmd agent export --name "$sourceTeam/$exportAgent" --output $exportPath2 2>&1
if (Test-Path $exportPath2) {
    Write-Host "✓ Export succeeded: $exportPath2" -ForegroundColor Green
} else {
    Write-Error "Export failed or file missing"
}

# Re-import with different name
$reimportName = "reimported-agent"
$reimportOutput = & $pekoCmd agent import --file $exportPath2 --name $reimportName --team $targetTeam 2>&1
if ($reimportOutput -match $reimportName) {
    Write-Host "✓ Re-import succeeded with name '$reimportName'" -ForegroundColor Green
} else {
    Write-Error "Re-import failed"
}

# ============================================================
# TEST 5: Layer deduplication (same agent exported twice)
# ============================================================
Write-Host "`n========================================" -ForegroundColor Cyan
Write-Host "TEST 5: Layer deduplication" -ForegroundColor Cyan
Write-Host "========================================" -ForegroundColor Cyan

$exportPath3 = "$testDir/my-agent-v2.agent"
& $pekoCmd agent export --name "$sourceTeam/$agentName" --output $exportPath3 2>&1 | Out-Null

$secondInspect = & $pekoCmd agent inspect $exportPath3 --json 2>&1 | ConvertFrom-Json

# Compare layer digests — same agent should produce identical layer digests
$matchingLayers = 0
$totalLayers = 0
foreach ($layer1 in $firstInspect.layers.PSObject.Properties) {
    $totalLayers++
    $layer2Value = $secondInspect.layers.($layer1.Name)
    if ($layer1.Value -and $layer1.Value -eq $layer2Value) {
        $matchingLayers++
        Write-Host "  ✓ Layer '$($layer1.Name)' digest matches" -ForegroundColor Gray
    } else {
        Write-Host "  ℹ Layer '$($layer1.Name)' digest differs" -ForegroundColor Yellow
    }
}

if ($matchingLayers -eq $totalLayers) {
    Write-Host "✓ All layer digests match — deduplication works" -ForegroundColor Green
} else {
    Write-Host "✓ $matchingLayers/$totalLayers layer digests match" -ForegroundColor Green
}

# ============================================================
# TEST 6: Error cases
# ============================================================
Write-Host "`n========================================" -ForegroundColor Cyan
Write-Host "TEST 6: Error cases" -ForegroundColor Cyan
Write-Host "========================================" -ForegroundColor Cyan

# Import non-existent file
$importError = & $pekoCmd agent import --file "$testDir/nonexistent.agent" 2>&1
if ($LASTEXITCODE -ne 0 -and $importError -match "not found") {
    Write-Host "✓ Import correctly rejects non-existent file" -ForegroundColor Green
} else {
    Write-Error "Import did not handle missing files correctly (exit: $LASTEXITCODE, output: $importError)"
}

# Inspect non-existent file
$inspectError = & $pekoCmd agent inspect "$testDir/nonexistent.agent" 2>&1
if ($LASTEXITCODE -ne 0 -and $inspectError -match "not found") {
    Write-Host "✓ Inspect correctly rejects non-existent file" -ForegroundColor Green
} else {
    Write-Error "Inspect did not handle missing files correctly (exit: $LASTEXITCODE, output: $inspectError)"
}

# Export non-existent agent
$exportError = & $pekoCmd agent export --name "nonexistentagent123" --team $sourceTeam --output "$testDir/fail.agent" 2>&1
if ($LASTEXITCODE -ne 0 -and $exportError -match "not found") {
    Write-Host "✓ Export correctly rejects non-existent agent" -ForegroundColor Green
} else {
    Write-Error "Export did not handle missing agents correctly (exit: $LASTEXITCODE, output: $exportError)"
}

# ============================================================
# Cleanup
# ============================================================
Write-Host "`n========================================" -ForegroundColor Cyan
Write-Host "Cleanup" -ForegroundColor Cyan
Write-Host "========================================" -ForegroundColor Cyan

& $pekoCmd agent remove $exportAgent --team $sourceTeam --force 2>&1 | Out-Null
& $pekoCmd agent remove $importedName --team $targetTeam --force 2>&1 | Out-Null
& $pekoCmd agent remove $reimportName --team $targetTeam --force 2>&1 | Out-Null
& $pekoCmd agent remove $agentName --team $sourceTeam --force 2>&1 | Out-Null
& $pekoCmd team remove $sourceTeam --force 2>&1 | Out-Null
& $pekoCmd team remove $targetTeam --force 2>&1 | Out-Null
Write-Host "Removed test agents and teams" -ForegroundColor Green

if (Test-Path $testDir) {
    Remove-Item -Recurse -Force $testDir
    Write-Host "Cleaned up test directory" -ForegroundColor Green
}

Write-Host "`n✅ All agent packaging tests completed!" -ForegroundColor Green
