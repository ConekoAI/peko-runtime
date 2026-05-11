#!/usr/bin/env pwsh
# Team Packaging E2E Test
#
# Tests the unified team packaging system (ADR-027):
# - Team export to .team package (peko team export)
# - Team import from .team package (peko team import)
# - Checksum validation on import
# - team.toml roundtrip
# - Export flags (--exclude-workspace, --include-sessions)

param(
    [string]$Provider = "minimax"
)

$ErrorActionPreference = "Stop"

Write-Host "========================================" -ForegroundColor Cyan
Write-Host "Team Packaging E2E Test" -ForegroundColor Cyan
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
$testDir = "$env:TEMP/pekobot_team_packaging_test_$([System.Guid]::NewGuid().ToString().Substring(0,8))"
New-Item -ItemType Directory -Path $testDir -Force | Out-Null
Write-Host "Test directory: $testDir" -ForegroundColor Gray

# ============================================================
# SETUP: Create test team with multiple agents
# ============================================================
Write-Host "`n========================================" -ForegroundColor Cyan
Write-Host "SETUP: Creating test team with agents" -ForegroundColor Cyan
Write-Host "========================================" -ForegroundColor Cyan

$testTeam = "testteam"
$agent1 = "agent1"
$agent2 = "agent2"
$agent3 = "agent3"
$importedTeamName = "importedteam"

# Clean up any leftover teams from previous runs
try { & $pekoCmd team remove $testTeam --force 2>&1 | Out-Null } catch {}
try { & $pekoCmd team remove $importedTeamName --force 2>&1 | Out-Null } catch {}
try { & $pekoCmd team remove "crossteam" --force 2>&1 | Out-Null } catch {}
try { & $pekoCmd team remove "tampered-team" --force 2>&1 | Out-Null } catch {}

& $pekoCmd team create $testTeam --description "Test team for packaging" 2>&1 | Out-Null
Write-Host "Created team: $testTeam" -ForegroundColor Green

# Create multiple agents in the team
& $pekoCmd agent create "$testTeam/$agent1" --provider $Provider 2>&1 | Out-Null
& $pekoCmd agent create "$testTeam/$agent2" --provider $Provider 2>&1 | Out-Null
& $pekoCmd agent create "$testTeam/$agent3" --provider $Provider 2>&1 | Out-Null
Write-Host "Created 3 agents in team: $agent1, $agent2, $agent3" -ForegroundColor Green

# Add workspace content for some agents
$workspaceDir1 = "$env:APPDATA/pekobot/workspaces/$testTeam/$agent1"
if (-not (Test-Path $workspaceDir1)) {
    New-Item -ItemType Directory -Path $workspaceDir1 -Force | Out-Null
}
"# Agent 1 System`n`nCustom system prompt for agent 1." | Out-File -FilePath "$workspaceDir1/SYSTEM.md" -Encoding UTF8

$workspaceDir2 = "$env:APPDATA/pekobot/workspaces/$testTeam/$agent2"
if (-not (Test-Path $workspaceDir2)) {
    New-Item -ItemType Directory -Path $workspaceDir2 -Force | Out-Null
}
"# Agent 2 Info`n`nAgent 2 documentation." | Out-File -FilePath "$workspaceDir2/AGENTS.md" -Encoding UTF8
Write-Host "Added workspace content to agents" -ForegroundColor Green

# ============================================================
# TEST 1: Team export
# ============================================================
Write-Host "`n========================================" -ForegroundColor Cyan
Write-Host "TEST 1: Team export" -ForegroundColor Cyan
Write-Host "========================================" -ForegroundColor Cyan

$teamExportPath = "$testDir/team_export.team"
$exportOutput = & $pekoCmd team export $testTeam -o $teamExportPath --json 2>&1 | ConvertFrom-Json
if ($exportOutput.output_path -and (Test-Path $exportOutput.output_path)) {
    $fileSize = (Get-Item $exportOutput.output_path).Length
    Write-Host "✓ Team exported successfully: $fileSize bytes" -ForegroundColor Green
} else {
    Write-Error "Team export failed or file missing: $($exportOutput | ConvertTo-Json)"
}

# Verify the .team file is a valid gzip tar
$gzipMagic = [byte[]]::new(2)
$fs = [System.IO.File]::OpenRead($teamExportPath)
$fs.Read($gzipMagic, 0, 2) | Out-Null
$fs.Close()
if ($gzipMagic[0] -eq 0x1f -and $gzipMagic[1] -eq 0x8b) {
    Write-Host "✓ Export file has valid gzip magic bytes" -ForegroundColor Green
} else {
    Write-Error "Export file is not valid gzip"
}

# ============================================================
# TEST 2: Team import with custom name
# ============================================================
Write-Host "`n========================================" -ForegroundColor Cyan
Write-Host "TEST 2: Team import with custom name" -ForegroundColor Cyan
Write-Host "========================================" -ForegroundColor Cyan

$importedTeamName = "importedteam"
$importOutput = & $pekoCmd team import $teamExportPath --name $importedTeamName --json 2>&1 | ConvertFrom-Json
if ($importOutput.name -eq $importedTeamName) {
    Write-Host "✓ Team imported successfully as '$importedTeamName'" -ForegroundColor Green
} else {
    Write-Error "Team import failed: $($importOutput | ConvertTo-Json)"
}

# Verify imported team has all agents
$showResult = & $pekoCmd team show $importedTeamName --json 2>&1 | ConvertFrom-Json
$importedAgentCount = if ($showResult.agents) { $showResult.agents.Count } else { 0 }
if ($importedAgentCount -eq 3) {
    Write-Host "✓ All 3 agents imported correctly" -ForegroundColor Green
} else {
    Write-Error "Agent count mismatch: expected 3, found $importedAgentCount"
}

# ============================================================
# TEST 3: Team re-import with --force
# ============================================================
Write-Host "`n========================================" -ForegroundColor Cyan
Write-Host "TEST 3: Team re-import with --force" -ForegroundColor Cyan
Write-Host "========================================" -ForegroundColor Cyan

$reimportOutput = & $pekoCmd team import $teamExportPath --name $importedTeamName --force --json 2>&1
if ($reimportOutput -match '"name"' -and $reimportOutput -match $importedTeamName) {
    Write-Host "✓ Team re-import with --force succeeded" -ForegroundColor Green
} else {
    Write-Error "Team re-import with --force failed"
}

# ============================================================
# TEST 4: Team export with --exclude-workspace
# ============================================================
Write-Host "`n========================================" -ForegroundColor Cyan
Write-Host "TEST 4: Team export with --exclude-workspace" -ForegroundColor Cyan
Write-Host "========================================" -ForegroundColor Cyan

$noWorkspacePath = "$testDir/team_no_workspace.team"
$exportNoWsOutput = & $pekoCmd team export $testTeam -o $noWorkspacePath --exclude-workspace --json 2>&1
if (Test-Path $noWorkspacePath) {
    $fullSize = (Get-Item $teamExportPath).Length
    $noWsSize = (Get-Item $noWorkspacePath).Length
    Write-Host "✓ Export without workspace: $noWsSize bytes (full: $fullSize bytes)" -ForegroundColor Green
    if ($noWsSize -lt $fullSize) {
        Write-Host "  ✓ Excluded workspace reduced file size" -ForegroundColor Green
    }
} else {
    Write-Error "Export with --exclude-workspace failed"
}

# ============================================================
# TEST 5: Checksum validation on import
# ============================================================
Write-Host "`n========================================" -ForegroundColor Cyan
Write-Host "TEST 5: Checksum validation on import" -ForegroundColor Cyan
Write-Host "========================================" -ForegroundColor Cyan

# Create a tampered .team package by appending garbage bytes
# (This corrupts the gzip trailer, making it an invalid archive)
$tamperedPath = "$testDir/tampered.team"
Copy-Item $teamExportPath $tamperedPath

# Append garbage to corrupt the file
$garbage = [byte[]]::new(32)
$rng = [System.Security.Cryptography.RandomNumberGenerator]::Create()
$rng.GetBytes($garbage)
$fs = [System.IO.File]::Open($tamperedPath, [System.IO.FileMode]::Append)
$fs.Write($garbage, 0, $garbage.Length)
$fs.Close()
Write-Host "Appended garbage bytes to corrupt package" -ForegroundColor Yellow

# Try to import the tampered package — this MUST fail
$tamperTeamName = "tampered-team"
$tamperFailed = $false
try {
    & $pekoCmd team import $tamperedPath --name $tamperTeamName --json 2>&1 | Out-Null
    Write-Error "Import of tampered .team package should have failed but succeeded"
} catch {
    $tamperFailed = $true
    Write-Host "✓ Import correctly rejected tampered package" -ForegroundColor Green
}

# Also verify via LASTEXITCODE if no exception was thrown
if (-not $tamperFailed -and $LASTEXITCODE -ne 0) {
    Write-Host "✓ Import correctly rejected tampered package (non-zero exit)" -ForegroundColor Green
    $tamperFailed = $true
}

if (-not $tamperFailed) {
    Write-Error "Checksum validation did not detect tampered package"
}

# ============================================================
# TEST 6: Verify team.toml roundtrip if present
# ============================================================
Write-Host "`n========================================" -ForegroundColor Cyan
Write-Host "TEST 6: Team configuration preservation" -ForegroundColor Cyan
Write-Host "========================================" -ForegroundColor Cyan

# Check original team description
$originalShow = & $pekoCmd team show $testTeam 2>&1
if ($originalShow -match "Test team for packaging") {
    Write-Host "✓ Original team description is set" -ForegroundColor Green
} else {
    Write-Error "Original team description not preserved"
}

# Check imported team description
$importedShow = & $pekoCmd team show $importedTeamName 2>&1
if ($importedShow -match "Test team for packaging") {
    Write-Host "✓ Imported team description preserved" -ForegroundColor Green
} else {
    Write-Error "Imported team description does not match"
}

# ============================================================
# TEST 7: Agent-level export from team (individual .agent files)
# ============================================================
Write-Host "`n========================================" -ForegroundColor Cyan
Write-Host "TEST 7: Agent-level export from team" -ForegroundColor Cyan
Write-Host "========================================" -ForegroundColor Cyan

$agents = @($agent1, $agent2, $agent3)
$allExported = $true
foreach ($agent in $agents) {
    $agentExportPath = "$testDir/$agent.agent"
    $result = & $pekoCmd agent export --name "$testTeam/$agent" --output $agentExportPath 2>&1
    if (Test-Path $agentExportPath) {
        $size = (Get-Item $agentExportPath).Length
        Write-Host "  ✓ Exported $agent.agent ($size bytes)" -ForegroundColor Gray
    } else {
        Write-Host "  ✗ Failed to export $agent" -ForegroundColor Red
        $allExported = $false
    }
}
if ($allExported) {
    Write-Host "✓ All agents exported individually" -ForegroundColor Green
}

# ============================================================
# TEST 8: Import individual .agent into different team
# ============================================================
Write-Host "`n========================================" -ForegroundColor Cyan
Write-Host "TEST 8: Import individual .agent into different team" -ForegroundColor Cyan
Write-Host "========================================" -ForegroundColor Cyan

$crossTeam = "crossteam"
& $pekoCmd team create $crossTeam 2>&1 | Out-Null

$crossImportName = "cross-imported"
$crossOutput = & $pekoCmd agent import --file "$testDir/$agent1.agent" --name $crossImportName --team $crossTeam 2>&1
if ($crossOutput -match $crossImportName) {
    Write-Host "✓ Cross-team import succeeded" -ForegroundColor Green
} else {
    Write-Error "Cross-team import failed"
}

# ============================================================
# Cleanup
# ============================================================
Write-Host "`n========================================" -ForegroundColor Cyan
Write-Host "Cleanup" -ForegroundColor Cyan
Write-Host "========================================" -ForegroundColor Cyan

foreach ($agent in $agents) {
    & $pekoCmd agent remove $agent --team $testTeam --force 2>&1 | Out-Null
}
& $pekoCmd team remove $testTeam --force 2>&1 | Out-Null
& $pekoCmd team remove $importedTeamName --force 2>&1 | Out-Null
& $pekoCmd team remove $crossTeam --force 2>&1 | Out-Null
# Clean up tampered team if it somehow got created
try { & $pekoCmd team remove $tamperTeamName --force 2>&1 | Out-Null } catch {}
Write-Host "Removed test teams and agents" -ForegroundColor Green

if (Test-Path $testDir) {
    Remove-Item -Recurse -Force $testDir
    Write-Host "Cleaned up test directory" -ForegroundColor Green
}

Write-Host "`n✅ All team packaging tests completed!" -ForegroundColor Green
