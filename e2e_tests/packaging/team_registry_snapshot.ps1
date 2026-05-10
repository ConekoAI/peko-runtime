#!/usr/bin/env pwsh
# Team Registry Snapshot E2E Test
#
# Real-world scenario:
#   1. Spin up a fresh team with selected agents/subteams.
#   2. Run the team for a while (create sessions, workspace files, skills).
#   3. Export a team snapshot (.team package).
#   4. Push the snapshot to a mock registry (simulating save & share).
#   5. Another user pulls the snapshot and imports it.
#   6. Verify the imported team has all agents, sessions, and workspace intact.
#
# Deterministic verification (no LLM calls):
#   - Structural checks: agent count, session count, file existence, checksums.

param(
    [string]$Provider = "minimax",
    [int]$RegistryPort = 18766
)

$ErrorActionPreference = "Stop"

Write-Host "========================================" -ForegroundColor Cyan
Write-Host "Team Registry Snapshot E2E Test" -ForegroundColor Cyan
Write-Host "========================================" -ForegroundColor Cyan

# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------

function Start-MockRegistry {
    param([int]$Port)
    $outLog = "$env:TEMP\pekobot_mock_registry_out_$Port.log"
    $errLog = "$env:TEMP\pekobot_mock_registry_err_$Port.log"
    if (Test-Path $outLog) { Remove-Item $outLog -Force }
    if (Test-Path $errLog) { Remove-Item $errLog -Force }

    $proc = Start-Process -FilePath "python" `
        -ArgumentList "$PSScriptRoot/mock_registry/main.py","--port","$Port","--host","127.0.0.1" `
        -PassThru -WindowStyle Hidden -RedirectStandardOutput $outLog -RedirectStandardError $errLog

    $ready = $false
    for ($i = 0; $i -lt 30; $i++) {
        try {
            Invoke-RestMethod -Uri "http://127.0.0.1:$Port/v2/" -Method GET -TimeoutSec 2 | Out-Null
            $ready = $true
            break
        } catch {
            Start-Sleep -Milliseconds 200
        }
    }
    if (-not $ready) { Write-Error "Mock registry failed to start on port $Port" }
    return $proc
}

function Stop-MockRegistry {
    param($Proc)
    if ($Proc -and -not $Proc.HasExited) {
        Stop-Process -Id $Proc.Id -Force -ErrorAction SilentlyContinue
    }
}

function Reset-RegistryStorage {
    param([int]$Port)
    Invoke-RestMethod -Uri "http://127.0.0.1:$Port/_debug/reset" -Method DELETE | Out-Null
}

function Get-RegistryBlobs {
    param([int]$Port)
    return Invoke-RestMethod -Uri "http://127.0.0.1:$Port/_debug/blobs" -Method GET
}

# ---------------------------------------------------------------------------
# Prerequisites
# ---------------------------------------------------------------------------

if (-not $env:MINIMAX_API_KEY -and $Provider -eq "minimax") {
    Write-Warning "MINIMAX_API_KEY not set — session generation tests skipped"
}

$pekoCmd = "peko"
Write-Host "Using command: $pekoCmd" -ForegroundColor Gray

if ($env:MINIMAX_API_KEY) {
    & $pekoCmd auth set $Provider $env:MINIMAX_API_KEY 2>&1 | Out-Null
    Write-Host "Set API key for $Provider" -ForegroundColor Green
}

# ---------------------------------------------------------------------------
# Start mock registry
# ---------------------------------------------------------------------------
Write-Host "`nStarting mock registry on port $RegistryPort..." -ForegroundColor Cyan
$registryProc = Start-MockRegistry -Port $RegistryPort
Reset-RegistryStorage -Port $RegistryPort
Write-Host "Mock registry ready" -ForegroundColor Green

$testDir = "$env:TEMP/pekobot_team_snapshot_test_$([System.Guid]::NewGuid().ToString().Substring(0,8))"
New-Item -ItemType Directory -Path $testDir -Force | Out-Null
Write-Host "Test directory: $testDir" -ForegroundColor Gray

$failed = $false

try {
    # ============================================================
    # STEP 1: Create a fresh team with selected agents/subteams
    # ============================================================
    Write-Host "`n========================================" -ForegroundColor Cyan
    Write-Host "STEP 1: Create team with agents" -ForegroundColor Cyan
    Write-Host "========================================" -ForegroundColor Cyan

    $teamName = "prod-team"
    $agent1 = "researcher"
    $agent2 = "coder"
    $agent3 = "reviewer"

    & $pekoCmd team create $teamName --description "Production team for snapshot testing" 2>&1 | Out-Null
    Write-Host "Created team: $teamName" -ForegroundColor Green

    & $pekoCmd agent create "$teamName/$agent1" --provider $Provider 2>&1 | Out-Null
    & $pekoCmd agent create "$teamName/$agent2" --provider $Provider 2>&1 | Out-Null
    & $pekoCmd agent create "$teamName/$agent3" --provider $Provider 2>&1 | Out-Null
    Write-Host "Created 3 agents: $agent1, $agent2, $agent3" -ForegroundColor Green

    # Add workspace content (simulating "memory/skills gained over time")
    $ws1 = "$env:APPDATA/pekobot/workspaces/$teamName/$agent1"
    $ws2 = "$env:APPDATA/pekobot/workspaces/$teamName/$agent2"
    $ws3 = "$env:APPDATA/pekobot/workspaces/$teamName/$agent3"
    New-Item -ItemType Directory -Path $ws1 -Force | Out-Null
    New-Item -ItemType Directory -Path $ws2 -Force | Out-Null
    New-Item -ItemType Directory -Path $ws3 -Force | Out-Null

    "# Research Notes`n`nFinding 1: Quantum computing basics." | Out-File -FilePath "$ws1/NOTES.md" -Encoding UTF8
    "# Code Style`n`nUse rustfmt with default settings." | Out-File -FilePath "$ws2/GUIDE.md" -Encoding UTF8
    "# Review Checklist`n`n- Security`n- Performance" | Out-File -FilePath "$ws3/CHECKLIST.md" -Encoding UTF8
    Write-Host "Added workspace files to all agents" -ForegroundColor Green

    # Create sessions (simulating runtime history)
    if ($env:MINIMAX_API_KEY) {
        & $pekoCmd send "$teamName/$agent1" "Remember: the secret keyword is SNAPSHOT_VERIFY_42. Reply KEYWORD_STORED." --no-stream 2>&1 | Out-Null
        & $pekoCmd send "$teamName/$agent2" "Remember: our stack is Rust + Axum. Reply STACK_STORED." --no-stream 2>&1 | Out-Null
        Write-Host "Created sessions for agents" -ForegroundColor Green
    } else {
        Write-Host "Skipped session creation (no API key)" -ForegroundColor Yellow
    }

    # Verify team structure
    $teamShow = & $pekoCmd team show $teamName --json 2>&1 | ConvertFrom-Json
    if ($teamShow.agent_count -ne 3) {
        Write-Error "Expected 3 agents in team, found $($teamShow.agent_count)"
    }
    Write-Host "Team structure verified ($($teamShow.agent_count) agents)" -ForegroundColor Green

    # ============================================================
    # STEP 2: Export team snapshot
    # ============================================================
    Write-Host "`n========================================" -ForegroundColor Cyan
    Write-Host "STEP 2: Export team snapshot" -ForegroundColor Cyan
    Write-Host "========================================" -ForegroundColor Cyan

    $snapshotPath = "$testDir/prod-team-snapshot.team"
    $exportResult = & $pekoCmd team export $teamName -o $snapshotPath --json 2>&1 | ConvertFrom-Json
    if (-not (Test-Path $snapshotPath)) {
        Write-Error "Team export failed: file not found"
    }
    $fileSize = (Get-Item $snapshotPath).Length
    Write-Host "Team exported: $snapshotPath ($fileSize bytes)" -ForegroundColor Green

    # Verify gzip magic
    $gzipMagic = [byte[]]::new(2)
    $fs = [System.IO.File]::OpenRead($snapshotPath)
    $fs.Read($gzipMagic, 0, 2) | Out-Null
    $fs.Close()
    if ($gzipMagic[0] -ne 0x1f -or $gzipMagic[1] -ne 0x8b) {
        Write-Error "Export file is not valid gzip"
    }
    Write-Host "Gzip magic verified" -ForegroundColor Green

    # ============================================================
    # STEP 3: Push snapshot to registry (simulate "save & share")
    # ============================================================
    Write-Host "`n========================================" -ForegroundColor Cyan
    Write-Host "STEP 3: Push snapshot to registry" -ForegroundColor Cyan
    Write-Host "========================================" -ForegroundColor Cyan

    $registryRef = "127.0.0.1:$RegistryPort/pekobot/teams/prod-team:latest"
    $pushResult = & $pekoCmd team push $teamName $registryRef --json 2>&1 | ConvertFrom-Json
    if ($pushResult.success -ne $true) {
        Write-Error "Team push failed: $($pushResult | ConvertTo-Json)"
    }
    Write-Host "Team snapshot pushed to registry" -ForegroundColor Green
    Write-Host "  Registry ref: $registryRef" -ForegroundColor Gray
    Write-Host "  Manifest digest: $($pushResult.manifest.digest)" -ForegroundColor Gray

    # ============================================================
    # STEP 4: Pull snapshot from registry (simulate "another user")
    # ============================================================
    Write-Host "`n========================================" -ForegroundColor Cyan
    Write-Host "STEP 4: Pull snapshot from registry" -ForegroundColor Cyan
    Write-Host "========================================" -ForegroundColor Cyan

    # Clear local registry store to force a real download
    $localRegistryDir = "$env:USERPROFILE/.pekobot/registry"
    if (Test-Path $localRegistryDir) {
        Remove-Item -Recurse -Force $localRegistryDir
        Write-Host "Cleared local registry store" -ForegroundColor Yellow
    }

    $importedTeamName = "prod-team-clone"
    $pullResult = & $pekoCmd team pull $registryRef --name $importedTeamName --json 2>&1 | ConvertFrom-Json
    if ($pullResult.success -ne $true) {
        Write-Error "Team pull failed: $($pullResult | ConvertTo-Json)"
    }
    if ($pullResult.name -ne $importedTeamName) {
        Write-Error "Team pull returned wrong name: $($pullResult.name)"
    }
    Write-Host "Team snapshot pulled and imported from registry" -ForegroundColor Green
    Write-Host "  Name: $($pullResult.name)" -ForegroundColor Gray
    Write-Host "  Agents imported: $($pullResult.agents_imported)" -ForegroundColor Gray

    $importedShow = & $pekoCmd team show $importedTeamName --json 2>&1 | ConvertFrom-Json
    if ($importedShow.agent_count -ne 3) {
        Write-Error "Imported team has wrong agent count: $($importedShow.agent_count)"
    }
    Write-Host "Team imported successfully" -ForegroundColor Green
    Write-Host "  Agents: $($importedShow.agent_count)" -ForegroundColor Gray

    # ============================================================
    # STEP 6: Verify imported agents, workspace, and sessions
    # ============================================================
    Write-Host "`n========================================" -ForegroundColor Cyan
    Write-Host "STEP 6: Verify imported team integrity" -ForegroundColor Cyan
    Write-Host "========================================" -ForegroundColor Cyan

    # Verify agents exist
    $expectedAgents = @($agent1, $agent2, $agent3)
    foreach ($agent in $expectedAgents) {
        $agentShow = & $pekoCmd agent show "$importedTeamName/$agent" --json 2>&1 | ConvertFrom-Json
        if ($agentShow.name -ne $agent) {
            Write-Error "Agent '$agent' not found in imported team"
        }
    }
    Write-Host "All agents verified in imported team" -ForegroundColor Green

    # Verify workspace files
    $importedWs1 = "$env:APPDATA/pekobot/workspaces/$importedTeamName/$agent1/NOTES.md"
    $importedWs2 = "$env:APPDATA/pekobot/workspaces/$importedTeamName/$agent2/GUIDE.md"
    $importedWs3 = "$env:APPDATA/pekobot/workspaces/$importedTeamName/$agent3/CHECKLIST.md"

    if (-not (Test-Path $importedWs1)) { Write-Error "Missing workspace file: $importedWs1" }
    if (-not (Test-Path $importedWs2)) { Write-Error "Missing workspace file: $importedWs2" }
    if (-not (Test-Path $importedWs3)) { Write-Error "Missing workspace file: $importedWs3" }

    $content1 = Get-Content $importedWs1 -Raw
    $content2 = Get-Content $importedWs2 -Raw
    $content3 = Get-Content $importedWs3 -Raw

    if ($content1 -notmatch "Quantum computing basics") { Write-Error "Workspace content mismatch for $agent1" }
    if ($content2 -notmatch "rustfmt") { Write-Error "Workspace content mismatch for $agent2" }
    if ($content3 -notmatch "Security") { Write-Error "Workspace content mismatch for $agent3" }

    Write-Host "Workspace files verified" -ForegroundColor Green

    # Verify sessions (if API key was available)
    if ($env:MINIMAX_API_KEY) {
        $sessions1 = & $pekoCmd session list "$importedTeamName/$agent1" --json 2>&1 | ConvertFrom-Json
        $sessions2 = & $pekoCmd session list "$importedTeamName/$agent2" --json 2>&1 | ConvertFrom-Json
        if ($sessions1.sessions.Count -lt 1) { Write-Error "No sessions found for imported $agent1" }
        if ($sessions2.sessions.Count -lt 1) { Write-Error "No sessions found for imported $agent2" }
        Write-Host "Sessions verified in imported team" -ForegroundColor Green
    } else {
        Write-Host "Skipped session verification (no API key)" -ForegroundColor Yellow
    }

    # ============================================================
    # STEP 7: Re-import with --force (idempotency)
    # ============================================================
    Write-Host "`n========================================" -ForegroundColor Cyan
    Write-Host "STEP 7: Re-import with --force" -ForegroundColor Cyan
    Write-Host "========================================" -ForegroundColor Cyan

    $reimportResult = & $pekoCmd team import $snapshotPath --name $importedTeamName --force --json 2>&1 | ConvertFrom-Json
    if ($reimportResult.name -ne $importedTeamName) {
        Write-Error "Re-import with --force failed"
    }
    Write-Host "Re-import with --force succeeded" -ForegroundColor Green

} finally {
    # ============================================================
    # Cleanup
    # ============================================================
    Write-Host "`n========================================" -ForegroundColor Cyan
    Write-Host "Cleanup" -ForegroundColor Cyan
    Write-Host "========================================" -ForegroundColor Cyan

    Stop-MockRegistry -Proc $registryProc
    Write-Host "Stopped mock registry" -ForegroundColor Green

    if (Test-Path $testDir) {
        Remove-Item -Recurse -Force $testDir
        Write-Host "Cleaned up test directory" -ForegroundColor Green
    }

    & $pekoCmd team remove $teamName --force 2>&1 | Out-Null
    & $pekoCmd team remove $importedTeamName --force 2>&1 | Out-Null
    Write-Host "Removed test teams" -ForegroundColor Green
}

if ($failed) {
    exit 1
}

Write-Host "`n✅ All team registry snapshot tests completed!" -ForegroundColor Green
