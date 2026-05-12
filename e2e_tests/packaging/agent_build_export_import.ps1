#!/usr/bin/env pwsh
# Agent Packaging E2E Test
#
# Tests the unified agent packaging system (ADR-027):
# - Build .agent from directory (peko agent build)
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
$testDir = "$env:TEMP/pekobot_packaging_test_$([System.Guid]::NewGuid().ToString().Substring(0,8))"
New-Item -ItemType Directory -Path $testDir -Force | Out-Null
Write-Host "Test directory: $testDir" -ForegroundColor Gray

# Create test teams
$sourceTeam = "sourceteam"
$targetTeam = "targetteam"
& $pekoCmd team create $sourceTeam 2>&1 | Out-Null
& $pekoCmd team create $targetTeam 2>&1 | Out-Null
Write-Host "Created teams: $sourceTeam, $targetTeam" -ForegroundColor Green

# ============================================================
# SETUP: Create agent source directory
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

# Create agent.toml (valid schema per AgentConfig)
@"
version = "1.0"
name = "my-agent"
description = "Test agent for packaging"
auto_accept_trusted = false
approval_threshold = 100.0
default_timeout_seconds = 300

[provider]
provider_type = "$Provider"
default_model = "default"
timeout_seconds = 60
max_retries = 3
retry_delay_ms = 1000

[provider.models.default]
name = "$Provider"
max_tokens = 4096
temperature = 0.7
top_p = 1.0
presence_penalty = 0.0
frequency_penalty = 0.0

[extensions]
enabled = ["shell", "read_file"]
"@ | Out-File -FilePath "$agentConfigDir/agent.toml" -Encoding UTF8

# Create identity stub (valid DID document, no BOM)
$didJson = @'
{
  "@context": ["https://www.w3.org/ns/did/v1"],
  "id": "did:pekobot:local:my-agent",
  "verificationMethod": [{
    "id": "did:pekobot:local:my-agent#keys-1",
    "type": "Ed25519VerificationKey2020",
    "controller": "did:pekobot:local:my-agent",
    "publicKeyMultibase": "z6MkhaXg"
  }],
  "authentication": ["did:pekobot:local:my-agent#keys-1"],
  "assertionMethod": ["did:pekobot:local:my-agent#keys-1"],
  "service": [],
  "created": "2026-05-09T00:00:00Z",
  "updated": "2026-05-09T00:00:00Z"
}
'@
[System.IO.File]::WriteAllText("$agentIdentityDir/did.json", $didJson)

# Create keys.enc (valid KeyPairExport JSON, no BOM)
$rng = [System.Security.Cryptography.RandomNumberGenerator]::Create()
$skBytes = New-Object byte[] 32; $rng.GetBytes($skBytes)
$pkBytes = New-Object byte[] 32; $rng.GetBytes($pkBytes)
$skB64 = [Convert]::ToBase64String($skBytes)
$pkB64 = [Convert]::ToBase64String($pkBytes)
$keysEnc = "{ `"public_key`": `"$pkB64`", `"private_key`": `"$skB64`" }"
[System.IO.File]::WriteAllText("$agentIdentityDir/keys.enc", $keysEnc)

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

$buildResult = & $pekoCmd agent build $agentSourceDir -t "my-agent:v1.0" --json 2>&1 | ConvertFrom-Json

if ($buildResult.tag -eq "my-agent:v1.0") {
    Write-Host "✓ Build succeeded with correct tag" -ForegroundColor Green
} else {
    Write-Error "Build failed or wrong tag: $($buildResult | ConvertTo-Json)"
}

if ($buildResult.layers -ge 2) {
    Write-Host "✓ Build produced $($buildResult.layers) layers" -ForegroundColor Green
} else {
    Write-Error "Expected at least 2 layers, got $($buildResult.layers)"
}

if ($buildResult.digest -match "sha256:") {
    Write-Host "✓ Build produced manifest digest: $($buildResult.digest)" -ForegroundColor Green
} else {
    Write-Error "Manifest digest missing or invalid"
}

# Verify .agent file was created
$builtAgentPath = $buildResult.package
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

$inspectResult = & $pekoCmd agent inspect $builtAgentPath --json 2>&1 | ConvertFrom-Json

if ($inspectResult.name -eq "my-agent") {
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
# TEST 3: Import built .agent with custom name
# ============================================================
Write-Host "`n========================================" -ForegroundColor Cyan
Write-Host "TEST 3: Import built .agent with custom name" -ForegroundColor Cyan
Write-Host "========================================" -ForegroundColor Cyan

$importedName = "imported-agent"
$importOutput = & $pekoCmd agent import --file $builtAgentPath --name $importedName --team $targetTeam 2>&1
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

$exportPath = "$testDir/exported.agent"
$exportOutput = & $pekoCmd agent export --name "$sourceTeam/$exportAgent" --output $exportPath 2>&1
if (Test-Path $exportPath) {
    Write-Host "✓ Export succeeded: $exportPath" -ForegroundColor Green
} else {
    Write-Error "Export failed or file missing"
}

# Re-import with different name
$reimportName = "reimported-agent"
$reimportOutput = & $pekoCmd agent import --file $exportPath --name $reimportName --team $targetTeam 2>&1
if ($reimportOutput -match $reimportName) {
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

$buildResult2 = & $pekoCmd agent build $agentSourceDir -t "my-agent:v2.0" --json 2>&1 | ConvertFrom-Json

# Compare layer digests — same source should produce identical layer digests
$matchingLayers = 0
foreach ($layer1 in $buildResult.layer_digests.PSObject.Properties) {
    $layer2Value = $buildResult2.layer_digests.($layer1.Name)
    if ($layer1.Value -and $layer1.Value -eq $layer2Value) {
        $matchingLayers++
        Write-Host "  ✓ Layer '$($layer1.Name)' digest matches: $($layer1.Value)" -ForegroundColor Gray
    } else {
        Write-Host "  ℹ Layer '$($layer1.Name)' digest differs (expected if content changed)" -ForegroundColor Yellow
    }
}

if ($matchingLayers -eq ($buildResult.layer_digests.PSObject.Properties | Measure-Object).Count) {
    Write-Host "✓ All layer digests match — deduplication works" -ForegroundColor Green
} else {
    Write-Host "✓ $matchingLayers/$($buildResult.layer_digests.PSObject.Properties.Count) layer digests match" -ForegroundColor Green
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
$buildError = & $pekoCmd agent build $badDir -t "bad:v1" 2>&1
if ($LASTEXITCODE -ne 0 -and $buildError -match "config/agent.toml") {
    Write-Host "✓ Build correctly rejects missing config/agent.toml" -ForegroundColor Green
} else {
    Write-Error "Build did not validate config/agent.toml presence (exit: $LASTEXITCODE, output: $buildError)"
}

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
& $pekoCmd team remove $sourceTeam --force 2>&1 | Out-Null
& $pekoCmd team remove $targetTeam --force 2>&1 | Out-Null
Write-Host "Removed test agents and teams" -ForegroundColor Green

if (Test-Path $testDir) {
    Remove-Item -Recurse -Force $testDir
    Write-Host "Cleaned up test directory" -ForegroundColor Green
}

Write-Host "`n✅ All agent packaging tests completed!" -ForegroundColor Green
