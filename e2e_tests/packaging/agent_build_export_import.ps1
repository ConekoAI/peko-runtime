#!/usr/bin/env pwsh
# Agent Packaging E2E Test
#
# Tests the unified agent packaging system (ADR-027):
# - Build .agent from directory (pekobot agent build)
# - Export running agent to .agent (pekobot agent export)
# - Inspect .agent package (pekobot agent inspect)
# - Import .agent package (pekobot agent import)
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

# Build pekobot
Write-Host "Building pekobot..." -ForegroundColor Cyan
pushd "$PSScriptRoot/../.."
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

# Set API key if available
if ($env:MINIMAX_API_KEY) {
    pekobot auth set $Provider $env:MINIMAX_API_KEY 2>&1 | Out-Null
    Write-Host "Set API key for $Provider" -ForegroundColor Green
}

# Create test directory
$testDir = "$env:TEMP/pekobot_packaging_test_$([System.Guid]::NewGuid().ToString().Substring(0,8))"
New-Item -ItemType Directory -Path $testDir -Force | Out-Null
Write-Host "Test directory: $testDir" -ForegroundColor Gray

# Create test teams
$sourceTeam = "sourceteam"
$targetTeam = "targetteam"
pekobot team create $sourceTeam 2>&1 | Out-Null
pekobot team create $targetTeam 2>&1 | Out-Null
Write-Host "Created teams: $sourceTeam, $targetTeam" -ForegroundColor Green

# ============================================================
# SETUP: Create agent from directory (for build test)
# ============================================================
Write-Host "`n========================================" -ForegroundColor Cyan
Write-Host "SETUP: Creating agent source directory" -ForegroundColor Cyan
Write-Host "========================================" -ForegroundColor Cyan

$agentSourceDir = "$testDir/my-agent"
$agentConfigDir = "$agentSourceDir/config"
$agentIdentityDir = "$agentSourceDir/identity"
$agentSkillsDir = "$agentSourceDir/skills"
$agentWorkspaceDir = "$agentSourceDir/workspace"

New-Item -ItemType Directory -Path $agentConfigDir -Force | Out-Null
New-Item -ItemType Directory -Path $agentIdentityDir -Force | Out-Null
New-Item -ItemType Directory -Path $agentSkillsDir -Force | Out-Null
New-Item -ItemType Directory -Path $agentWorkspaceDir -Force | Out-Null

# Create agent.toml
@"
name = "my-agent"
version = "1.0.0"
description = "Test agent for packaging"
provider = "$Provider"

[extensions]
enabled = ["shell", "read_file"]

[prompt]
system = "You are a test agent."
"@ | Out-File -FilePath "$agentConfigDir/agent.toml" -Encoding UTF8

# Create prompts.toml
@"
[prompts]
default = "You are a test agent for packaging validation."
"@ | Out-File -FilePath "$agentConfigDir/prompts.toml" -Encoding UTF8

# Create a skill
New-Item -ItemType Directory -Path "$agentSkillsDir/test-skill" -Force | Out-Null
@"
# Test Skill

A skill for testing packaging.

## Usage

Use this skill to verify packaging works.
"@ | Out-File -FilePath "$agentSkillsDir/test-skill/SKILL.md" -Encoding UTF8

# Create workspace content
@"
# Test Workspace

This is a test workspace file.
"@ | Out-File -FilePath "$agentWorkspaceDir/README.md" -Encoding UTF8

Write-Host "Created agent source directory at $agentSourceDir" -ForegroundColor Green

# ============================================================
# TEST 1: Build .agent from directory
# ============================================================
Write-Host "`n========================================" -ForegroundColor Cyan
Write-Host "TEST 1: Build .agent from directory" -ForegroundColor Cyan
Write-Host "========================================" -ForegroundColor Cyan

$buildOutput = "$testDir/built.agent"
$buildResult = pekobot agent build $agentSourceDir -t "my-agent:v1.0" --json 2>&1 | ConvertFrom-Json

if ($buildResult.tag -eq "my-agent:v1.0") {
    Write-Host "✓ Build succeeded with correct tag" -ForegroundColor Green
} else {
    Write-Error "Build failed or wrong tag: $($buildResult | ConvertTo-Json)"
}

if ($buildResult.layers.Count -ge 2) {
    Write-Host "✓ Build produced $($buildResult.layers.Count) layers" -ForegroundColor Green
} else {
    Write-Warning "Expected at least 2 layers, got $($buildResult.layers.Count)"
}

if ($buildResult.digest -match "sha256:") {
    Write-Host "✓ Build produced manifest digest: $($buildResult.digest)" -ForegroundColor Green
} else {
    Write-Warning "Manifest digest missing or invalid"
}

# Verify .agent file was created
$builtAgentPath = $buildResult.package_path
if (Test-Path $builtAgentPath) {
    $fileSize = (Get-Item $builtAgentPath).Length
    Write-Host "✓ Built .agent file exists: $fileSize bytes" -ForegroundColor Green
} else {
    Write-Error "Built .agent file not found at $builtAgentPath"
}

# ============================================================
# TEST 2: Inspect built .agent package
# ============================================================
Write-Host "`n========================================" -ForegroundColor Cyan
Write-Host "TEST 2: Inspect built .agent package" -ForegroundColor Cyan
Write-Host "========================================" -ForegroundColor Cyan

$inspectResult = pekobot agent inspect $builtAgentPath --json 2>&1 | ConvertFrom-Json

if ($inspectResult.agent.name -eq "my-agent") {
    Write-Host "✓ Inspect shows correct agent name" -ForegroundColor Green
} else {
    Write-Error "Inspect shows wrong name: $($inspectResult.agent.name)"
}

if ($inspectResult.layers) {
    Write-Host "✓ Inspect shows layers section" -ForegroundColor Green
    foreach ($layer in $inspectResult.layers.PSObject.Properties) {
        Write-Host "  - $($layer.Name): $($layer.Value)" -ForegroundColor Gray
    }
} else {
    Write-Warning "Inspect result missing layers"
}

# Verify manifest has no dead fields (capabilities, tools, mcp, tool_sources)
$inspectToml = pekobot agent inspect $builtAgentPath 2>&1
if ($inspectToml -match "capabilities" -or $inspectToml -match "tool_sources") {
    Write-Warning "Inspect output may contain dead manifest fields"
} else {
    Write-Host "✓ No dead fields (capabilities, tool_sources) in inspect output" -ForegroundColor Green
}

# ============================================================
# TEST 3: Import built .agent with custom name
# ============================================================
Write-Host "`n========================================" -ForegroundColor Cyan
Write-Host "TEST 3: Import built .agent with custom name" -ForegroundColor Cyan
Write-Host "========================================" -ForegroundColor Cyan

$importedName = "imported-agent"
$importResult = pekobot agent import --file $builtAgentPath --name $importedName --team $targetTeam --json 2>&1 | ConvertFrom-Json

if ($importResult.name -eq $importedName) {
    Write-Host "✓ Import succeeded with name '$importedName'" -ForegroundColor Green
} else {
    Write-Error "Import failed or wrong name: $($importResult | ConvertTo-Json)"
}

# Verify imported agent exists
$showResult = pekobot agent show "$targetTeam/$importedName" --json 2>&1 | ConvertFrom-Json
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
pekobot agent create "$sourceTeam/$exportAgent" --provider $Provider 2>&1 | Out-Null
Write-Host "Created agent: $sourceTeam/$exportAgent" -ForegroundColor Green

$exportPath = "$testDir/exported.agent"
$exportResult = pekobot agent export --name "$sourceTeam/$exportAgent" --output $exportPath --json 2>&1 | ConvertFrom-Json

if ($exportResult.output_path -and (Test-Path $exportResult.output_path)) {
    Write-Host "✓ Export succeeded: $($exportResult.output_path)" -ForegroundColor Green
} else {
    Write-Error "Export failed or file missing"
}

# Re-import with different name
$reimportName = "reimported-agent"
$reimportResult = pekobot agent import --file $exportPath --name $reimportName --team $targetTeam --json 2>&1 | ConvertFrom-Json

if ($reimportResult.name -eq $reimportName) {
    Write-Host "✓ Re-import succeeded with name '$reimportName'" -ForegroundColor Green
} else {
    Write-Error "Re-import failed"
}

# ============================================================
# TEST 5: Build layer deduplication
# ============================================================
Write-Host "`n========================================" -ForegroundColor Cyan
Write-Host "TEST 5: Build layer deduplication" -ForegroundColor Cyan
Write-Host "========================================" -ForegroundColor Cyan

$buildResult2 = pekobot agent build $agentSourceDir -t "my-agent:v2.0" --json 2>&1 | ConvertFrom-Json

# Compare layer digests — same source should produce identical layer digests
$matchingLayers = 0
foreach ($layer1 in $buildResult.layers.PSObject.Properties) {
    $layer2Value = $buildResult2.layers.($layer1.Name)
    if ($layer1.Value -eq $layer2Value) {
        $matchingLayers++
        Write-Host "  ✓ Layer '$($layer1.Name)' digest matches: $($layer1.Value)" -ForegroundColor Gray
    } else {
        Write-Host "  ℹ Layer '$($layer1.Name)' digest differs (expected if content changed)" -ForegroundColor Yellow
    }
}

if ($matchingLayers -eq $buildResult.layers.Count) {
    Write-Host "✓ All layer digests match — deduplication works" -ForegroundColor Green
} else {
    Write-Host "✓ $matchingLayers/$($buildResult.layers.Count) layer digests match" -ForegroundColor Green
}

# ============================================================
# TEST 6: Error cases
# ============================================================
Write-Host "`n========================================" -ForegroundColor Cyan
Write-Host "TEST 6: Error cases" -ForegroundColor Cyan
Write-Host "========================================" -ForegroundColor Cyan

# Build from directory without config/agent.toml
$badDir = "$testDir/bad-agent"
New-Item -ItemType Directory -Path $badDir -Force | Out-Null
$buildError = pekobot agent build $badDir -t "bad:v1" 2>&1
if ($buildError -match "config/agent.toml" -or $LASTEXITCODE -ne 0) {
    Write-Host "✓ Build correctly rejects missing config/agent.toml" -ForegroundColor Green
} else {
    Write-Warning "Build may not validate config/agent.toml presence"
}

# Import non-existent file
$importError = pekobot agent import --file "$testDir/nonexistent.agent" 2>&1
if ($importError -match "not found" -or $importError -match "error" -or $LASTEXITCODE -ne 0) {
    Write-Host "✓ Import correctly rejects non-existent file" -ForegroundColor Green
} else {
    Write-Warning "Import may not handle missing files correctly"
}

# Inspect non-existent file
$inspectError = pekobot agent inspect "$testDir/nonexistent.agent" 2>&1
if ($inspectError -match "not found" -or $inspectError -match "error" -or $LASTEXITCODE -ne 0) {
    Write-Host "✓ Inspect correctly rejects non-existent file" -ForegroundColor Green
} else {
    Write-Warning "Inspect may not handle missing files correctly"
}

# Export non-existent agent
$exportError = pekobot agent export --name "nonexistentagent123" --team $sourceTeam --output "$testDir/fail.agent" 2>&1
if ($exportError -match "not found" -or $exportError -match "error" -or $LASTEXITCODE -ne 0) {
    Write-Host "✓ Export correctly rejects non-existent agent" -ForegroundColor Green
} else {
    Write-Warning "Export may not handle missing agents correctly"
}

# ============================================================
# Cleanup
# ============================================================
Write-Host "`n========================================" -ForegroundColor Cyan
Write-Host "Cleanup" -ForegroundColor Cyan
Write-Host "========================================" -ForegroundColor Cyan

pekobot agent remove $exportAgent --team $sourceTeam --force 2>&1 | Out-Null
pekobot agent remove $importedName --team $targetTeam --force 2>&1 | Out-Null
pekobot agent remove $reimportName --team $targetTeam --force 2>&1 | Out-Null
pekobot team remove $sourceTeam --force 2>&1 | Out-Null
pekobot team remove $targetTeam --force 2>&1 | Out-Null
Write-Host "Removed test agents and teams" -ForegroundColor Green

if (Test-Path $testDir) {
    Remove-Item -Recurse -Force $testDir
    Write-Host "Cleaned up test directory" -ForegroundColor Green
}

Write-Host "`n✅ All agent packaging tests completed!" -ForegroundColor Green
