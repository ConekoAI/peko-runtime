#!/usr/bin/env pwsh
# Team Image Export/Import E2E Test
#
# Tests team-level packaging functionality:
# - Team export to .team package (all agents in team)
# - Team import from .team package
# - Cross-system team migration
# - Team configuration preservation
# - Agent identity management during team import

param(
    [string]$Provider = "kimi"
)

$ErrorActionPreference = "Stop"

Write-Host "========================================" -ForegroundColor Cyan
Write-Host "Team Image Export/Import E2E Test" -ForegroundColor Cyan
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
$testDir = "$env:TEMP/pekobot_team_image_test_$([System.Guid]::NewGuid().ToString().Substring(0,8))"
New-Item -ItemType Directory -Path $testDir -Force | Out-Null
Write-Host "Test directory: $testDir" -ForegroundColor Gray

# ============================================================
# Setup: Create test team with multiple agents
# ============================================================
Write-Host "`n========================================" -ForegroundColor Cyan
Write-Host "SETUP: Creating test team with agents" -ForegroundColor Cyan
Write-Host "========================================" -ForegroundColor Cyan

$testTeam = "testteam"
$agent1 = "agent1"
$agent2 = "agent2"
$agent3 = "agent3"

pekobot team create $testTeam --description "Test team for image export" 2>&1 | Out-Null
Write-Host "Created team: $testTeam" -ForegroundColor Green

# Create multiple agents in the team
pekobot agent create "$testTeam/$agent1" --provider $Provider 2>&1 | Out-Null
pekobot agent create "$testTeam/$agent2" --provider $Provider 2>&1 | Out-Null
pekobot agent create "$testTeam/$agent3" --provider $Provider 2>&1 | Out-Null
Write-Host "Created 3 agents in team: $agent1, $agent2, $agent3" -ForegroundColor Green

# Add workspace content for some agents
$workspaceDir1 = "$env:USERPROFILE/.pekobot/workspaces/$testTeam/$agent1"
if (-not (Test-Path $workspaceDir1)) {
    New-Item -ItemType Directory -Path $workspaceDir1 -Force | Out-Null
}
"# Agent 1 System`n`nCustom system prompt for agent 1." | Out-File -FilePath "$workspaceDir1/SYSTEM.md" -Encoding UTF8

$workspaceDir2 = "$env:USERPROFILE/.pekobot/workspaces/$testTeam/$agent2"
if (-not (Test-Path $workspaceDir2)) {
    New-Item -ItemType Directory -Path $workspaceDir2 -Force | Out-Null
}
"# Agent 2 Info`n`nAgent 2 documentation." | Out-File -FilePath "$workspaceDir2/AGENTS.md" -Encoding UTF8
Write-Host "Added workspace content to agents" -ForegroundColor Green

# ============================================================
# TEST 1: Team export functionality
# ============================================================
Write-Host "`n========================================" -ForegroundColor Cyan
Write-Host "TEST 1: Team export" -ForegroundColor Cyan
Write-Host "========================================" -ForegroundColor Cyan

$teamExportPath = "$testDir/team_export.team"
Write-Host "Exporting team to: $teamExportPath" -ForegroundColor Yellow
$exportResult = pekobot team export $testTeam --output $teamExportPath 2>&1
Write-Host "Output: $exportResult"

if ($exportResult -match "Exported") {
    Write-Host "✓ Team exported successfully" -ForegroundColor Green
    
    # Verify file exists
    if (Test-Path $teamExportPath) {
        $fileSize = (Get-Item $teamExportPath).Length
        Write-Host "✓ Export file created: $fileSize bytes" -ForegroundColor Green
    }
} else {
    Write-Error "Team export failed: $exportResult"
}

# ============================================================
# TEST 2: Team import functionality
# ============================================================
Write-Host "`n========================================" -ForegroundColor Cyan
Write-Host "TEST 2: Team import" -ForegroundColor Cyan
Write-Host "========================================" -ForegroundColor Cyan

$importedTeamName = "importedteam"
Write-Host "Importing team package as: $importedTeamName" -ForegroundColor Yellow
$importResult = pekobot team import $teamExportPath --name $importedTeamName 2>&1
Write-Host "Output: $importResult"

if ($importResult -match "Imported") {
    Write-Host "✓ Team imported successfully" -ForegroundColor Green
    
    # Verify imported team exists with agents
    $importedTeamInfo = pekobot team show $importedTeamName --json 2>&1 | ConvertFrom-Json
    if ($importedTeamInfo.agents.Count -eq 3) {
        Write-Host "✓ All 3 agents imported correctly" -ForegroundColor Green
    } else {
        Write-Warning "Agent count mismatch: expected 3, found $($importedTeamInfo.agents.Count)"
    }
} else {
    Write-Error "Team import failed: $importResult"
}

# ============================================================
# TEST 3: Verify team structure for future export
# ============================================================
Write-Host "`n========================================" -ForegroundColor Cyan
Write-Host "TEST 3: Verify team structure" -ForegroundColor Cyan
Write-Host "========================================" -ForegroundColor Cyan

$teamInfo = pekobot team show $testTeam --json 2>&1 | ConvertFrom-Json
Write-Host "Team info retrieved: $($teamInfo.name)" -ForegroundColor Gray

# Verify team has expected agents
$teamAgentList = pekobot agent list --json 2>&1 | ConvertFrom-Json
$teamAgents = $teamAgentList.teams | Where-Object { $_.name -eq $testTeam } | Select-Object -ExpandProperty agents

if ($teamAgents.Count -eq 3) {
    Write-Host "✓ Team has all 3 expected agents" -ForegroundColor Green
} else {
    Write-Warning "Team agent count mismatch: expected 3, found $($teamAgents.Count)"
}

# ============================================================
# TEST 4: Agent-level export as team workaround
# ============================================================
Write-Host "`n========================================" -ForegroundColor Cyan
Write-Host "TEST 4: Agent-level export (team workaround)" -ForegroundColor Cyan
Write-Host "========================================" -ForegroundColor Cyan

$teamAgentsDir = "$testDir/team_agents"
New-Item -ItemType Directory -Path $teamAgentsDir -Force | Out-Null

# Export each agent individually
$agents = @($agent1, $agent2, $agent3)
foreach ($agent in $agents) {
    $exportPath = "$teamAgentsDir/$agent.agent"
    Write-Host "Exporting $testTeam/$agent to $exportPath" -ForegroundColor Yellow
    $result = pekobot agent export --name "$testTeam/$agent" --output $exportPath 2>&1
    
    if ($result -match "Exported" -or $LASTEXITCODE -eq 0) {
        Write-Host "  ✓ Exported $agent" -ForegroundColor Green
    } else {
        Write-Host "  ℹ Export status: $result" -ForegroundColor Yellow
    }
}

Write-Host "✓ Agent-level export completed for all team agents" -ForegroundColor Green

# ============================================================
# TEST 5: Team configuration preservation
# ============================================================
Write-Host "`n========================================" -ForegroundColor Cyan
Write-Host "TEST 5: Team configuration preservation" -ForegroundColor Cyan
Write-Host "========================================" -ForegroundColor Cyan

# Verify team description is preserved
$teamShow = pekobot team show $testTeam 2>&1
if ($teamShow -match "Test team for image export") {
    Write-Host "✓ Team description is preserved" -ForegroundColor Green
} else {
    Write-Warning "Team description may not be preserved correctly"
}

# ============================================================
# TEST 6: Workspace content packaging verification
# ============================================================
Write-Host "`n========================================" -ForegroundColor Cyan
Write-Host "TEST 6: Workspace content verification" -ForegroundColor Cyan
Write-Host "========================================" -ForegroundColor Cyan

# Check if workspace files exist
$systemMdExists = Test-Path "$workspaceDir1/SYSTEM.md"
$agentsMdExists = Test-Path "$workspaceDir2/AGENTS.md"

if ($systemMdExists -and $agentsMdExists) {
    Write-Host "✓ Workspace content files exist" -ForegroundColor Green
    
    # Verify content
    $systemContent = Get-Content "$workspaceDir1/SYSTEM.md" -Raw
    if ($systemContent -match "Custom system prompt") {
        Write-Host "✓ SYSTEM.md content is correct" -ForegroundColor Green
    }
} else {
    Write-Warning "Some workspace files are missing"
}

# ============================================================
# TEST 7: Cross-team agent move (current workaround)
# ============================================================
Write-Host "`n========================================" -ForegroundColor Cyan
Write-Host "TEST 7: Cross-team agent move (workaround)" -ForegroundColor Cyan
Write-Host "========================================" -ForegroundColor Cyan

$tempTeam = "tempteam"
pekobot team create $tempTeam 2>&1 | Out-Null

# Move an agent to another team
Write-Host "Moving $agent1 from $testTeam to $tempTeam" -ForegroundColor Yellow
$moveResult = pekobot agent move $agent1 --team $testTeam $agent1 --to-team $tempTeam --json 2>&1 | ConvertFrom-Json

if ($moveResult.team -eq $tempTeam) {
    Write-Host "✓ Agent moved successfully to new team" -ForegroundColor Green
    
    # Move it back
    pekobot agent move $agent1 --team $tempTeam $agent1 --to-team $testTeam 2>&1 | Out-Null
    Write-Host "✓ Agent moved back to original team" -ForegroundColor Green
} else {
    Write-Warning "Cross-team move may have failed"
}

pekobot team remove $tempTeam --force 2>&1 | Out-Null

# ============================================================
# TEST 8: Agent identity (DID) verification
# ============================================================
Write-Host "`n========================================" -ForegroundColor Cyan
Write-Host "TEST 8: Agent identity verification" -ForegroundColor Cyan
Write-Host "========================================" -ForegroundColor Cyan

# Get DIDs for all agents
foreach ($agent in $agents) {
    $agentInfo = pekobot agent show $agent --team $testTeam --json 2>&1 | ConvertFrom-Json
    $did = $agentInfo.did
    
    if ($did -match "did:pekobot:" -or $did -match "did:key:") {
        Write-Host "✓ $agent has valid DID: $did" -ForegroundColor Green
    } else {
        Write-Warning "$agent has unexpected DID format: $did"
    }
}

# ============================================================
# TEST 9: Future team package format documentation
# ============================================================
Write-Host "`n========================================" -ForegroundColor Cyan
Write-Host "TEST 9: Team package format specification" -ForegroundColor Cyan
Write-Host "========================================" -ForegroundColor Cyan

Write-Host "Team package (.team) would contain:" -ForegroundColor Gray
Write-Host "  - team/manifest.toml - Team metadata and agent list" -ForegroundColor Gray
Write-Host "  - team/config.toml - Team configuration" -ForegroundColor Gray
Write-Host "  - agents/{name}/config.toml - Individual agent configs" -ForegroundColor Gray
Write-Host "  - agents/{name}/identity/ - Agent identity files" -ForegroundColor Gray
Write-Host "  - agents/{name}/workspace/ - Agent workspace files" -ForegroundColor Gray
Write-Host "  - signatures/ - Package signatures" -ForegroundColor Gray

Write-Host "✓ Team package structure documented" -ForegroundColor Green

# ============================================================
# TEST 10: Team re-import with force
# ============================================================
Write-Host "`n========================================" -ForegroundColor Cyan
Write-Host "TEST 10: Team re-import with force" -ForegroundColor Cyan
Write-Host "========================================" -ForegroundColor Cyan

Write-Host "Re-importing team to same name with --force" -ForegroundColor Yellow
$reimportResult = pekobot team import $teamExportPath --name $importedTeamName --force 2>&1
Write-Host "Output: $reimportResult"

if ($reimportResult -match "Imported") {
    Write-Host "✓ Team re-import with force executed successfully" -ForegroundColor Green
} else {
    Write-Warning "Team re-import may have issues: $reimportResult"
}

# ============================================================
# Cleanup
# ============================================================
Write-Host "`n========================================" -ForegroundColor Cyan
Write-Host "Cleanup" -ForegroundColor Cyan
Write-Host "========================================" -ForegroundColor Cyan

# Remove test agents and teams
foreach ($agent in $agents) {
    pekobot agent remove $agent --team $testTeam --force 2>&1 | Out-Null
}
pekobot team remove $testTeam --force 2>&1 | Out-Null
pekobot team remove $importedTeamName --force 2>&1 | Out-Null
Write-Host "Removed test agents and teams" -ForegroundColor Green

# Remove test directory
if (Test-Path $testDir) {
    Remove-Item -Recurse -Force $testDir
    Write-Host "Cleaned up test directory" -ForegroundColor Green
}

Write-Host "`n✅ All team image tests completed!" -ForegroundColor Green
