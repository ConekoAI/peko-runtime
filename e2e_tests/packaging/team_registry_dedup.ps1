#!/usr/bin/env pwsh
# Team Registry Deduplication E2E Test
#
# Verifies that pushing two teams sharing the same agent results in
# deduplicated layer storage on the registry.
#
# Scenario:
#   1. Create Team A with Agent X.
#   2. Push Team A to mock registry → note blob count.
#   3. Create Team B with Agent X (same config/content).
#   4. Push Team B to mock registry → verify blob count increased by
#      only the TeamConfig layer size (agent layers are skipped).
#   5. Pull Team B → verify imports correctly.
#   6. Verify Agent X functions identically in both teams.
#
# Deterministic verification (no LLM calls):
#   - Blob count comparison before/after second push.
#   - Structural checks: agent count, file existence.

param(
    [string]$Provider = "minimax",
    [int]$RegistryPort = 18776
)

$ErrorActionPreference = "Stop"

Write-Host "========================================" -ForegroundColor Cyan
Write-Host "Team Registry Deduplication E2E Test" -ForegroundColor Cyan
Write-Host "========================================" -ForegroundColor Cyan

# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------

function Start-MockRegistry {
    param([int]$Port)
    $outLog = "$env:TEMP\PEKO_mock_registry_out_$Port.log"
    $errLog = "$env:TEMP\PEKO_mock_registry_err_$Port.log"
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

$repoRoot = Resolve-Path (Join-Path $PSScriptRoot "../../..")
$builtBinary = Join-Path $repoRoot "peko-runtime/target/debug/peko.exe"
if (Test-Path $builtBinary) {
    $pekoCmd = $builtBinary
} else {
    $pekoCmd = "peko"
}
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

$registryHost = "127.0.0.1:$RegistryPort"
& $pekoCmd login --api-key "mock_test_token" --registry $registryHost 2>&1 | Out-Null

$testDir = "$env:TEMP/PEKO_team_dedup_test_$([System.Guid]::NewGuid().ToString().Substring(0,8))"
New-Item -ItemType Directory -Path $testDir -Force | Out-Null
Write-Host "Test directory: $testDir" -ForegroundColor Gray

$failed = $false

try {
    # ============================================================
    # STEP 1: Create Team A with Agent X
    # ============================================================
    Write-Host "`n========================================" -ForegroundColor Cyan
    Write-Host "STEP 1: Create Team A with Agent X" -ForegroundColor Cyan
    Write-Host "========================================" -ForegroundColor Cyan

    $teamA = "team-alpha"
    $agentX = "shared-agent"

    & $pekoCmd team create $teamA --description "Team A for dedup testing" 2>&1 | Out-Null
    & $pekoCmd agent create "$teamA/$agentX" --provider $Provider 2>&1 | Out-Null
    Write-Host "Created Team A with agent '$agentX'" -ForegroundColor Green

    # Add workspace content to make the agent more realistic
    $wsA = "$env:APPDATA/peko/workspaces/$teamA/$agentX"
    New-Item -ItemType Directory -Path $wsA -Force | Out-Null
    "# Agent X Notes`nShared workspace content." | Out-File -FilePath "$wsA/NOTES.md" -Encoding UTF8
    Write-Host "Added workspace content to Agent X in Team A" -ForegroundColor Green

    # ============================================================
    # STEP 2: Push Team A to registry
    # ============================================================
    Write-Host "`n========================================" -ForegroundColor Cyan
    Write-Host "STEP 2: Push Team A to registry" -ForegroundColor Cyan
    Write-Host "========================================" -ForegroundColor Cyan

    $registryRefA = "127.0.0.1:$RegistryPort/peko/teams/team-alpha:latest"
    $pushA = & $pekoCmd team push $teamA $registryRefA --json 2>&1 | ConvertFrom-Json
    if ($pushA.success -ne $true) {
        Write-Error "Team A push failed: $($pushA | ConvertTo-Json)"
    }

    $stateAfterA = Get-RegistryBlobs -Port $RegistryPort
    $blobsAfterA = $stateAfterA.blobs.Count
    Write-Host "Team A pushed successfully" -ForegroundColor Green
    Write-Host "  Registry blobs after Team A: $blobsAfterA" -ForegroundColor Gray
    Write-Host "  Manifest layers: $($pushA.manifest.layers)" -ForegroundColor Gray

    # A team with 1 agent should have at least: TeamConfig + Config + Identity layers
    # (Workspace is optional depending on whether it's empty)
    if ($pushA.manifest.layers -lt 3) {
        Write-Error "Expected at least 3 layers (TeamConfig + Config + Identity), got $($pushA.manifest.layers)"
    }

    # ============================================================
    # STEP 3: Create Team B with the SAME Agent X (imported from Team A)
    # ============================================================
    Write-Host "`n========================================" -ForegroundColor Cyan
    Write-Host "STEP 3: Create Team B with same Agent X" -ForegroundColor Cyan
    Write-Host "========================================" -ForegroundColor Cyan

    # Export Agent X from Team A so Team B gets the exact same agent
    # (same identity, same config → identical layers → deduplication)
    $agentExportPath = "$testDir/shared-agent.agent"
    & $pekoCmd agent export --name "$teamA/$agentX" --output "$agentExportPath" 2>&1 | Out-Null
    if (-not (Test-Path $agentExportPath)) {
        Write-Error "Failed to export agent from Team A"
    }
    Write-Host "Exported Agent X from Team A" -ForegroundColor Green

    $teamB = "team-beta"

    & $pekoCmd team create $teamB --description "Team B for dedup testing" 2>&1 | Out-Null
    & $pekoCmd agent import --file "$agentExportPath" --name $agentX --team $teamB 2>&1 | Out-Null
    $verifyImport = & $pekoCmd agent show "$teamB/$agentX" --json 2>&1 | ConvertFrom-Json
    if ($verifyImport.name -ne $agentX) {
        Write-Error "Failed to import agent into Team B"
    }
    Write-Host "Created Team B with imported agent '$agentX'" -ForegroundColor Green

    # Add the SAME workspace content (identical → same layer digest)
    $wsB = "$env:APPDATA/peko/workspaces/$teamB/$agentX"
    New-Item -ItemType Directory -Path $wsB -Force | Out-Null
    "# Agent X Notes`nShared workspace content." | Out-File -FilePath "$wsB/NOTES.md" -Encoding UTF8
    Write-Host "Added identical workspace content to Agent X in Team B" -ForegroundColor Green

    # ============================================================
    # STEP 4: Push Team B to registry
    # ============================================================
    Write-Host "`n========================================" -ForegroundColor Cyan
    Write-Host "STEP 4: Push Team B to registry" -ForegroundColor Cyan
    Write-Host "========================================" -ForegroundColor Cyan

    $registryRefB = "127.0.0.1:$RegistryPort/peko/teams/team-beta:latest"
    $pushB = & $pekoCmd team push $teamB $registryRefB --json 2>&1 | ConvertFrom-Json
    if ($pushB.success -ne $true) {
        Write-Error "Team B push failed: $($pushB | ConvertTo-Json)"
    }

    $stateAfterB = Get-RegistryBlobs -Port $RegistryPort
    $blobsAfterB = $stateAfterB.blobs.Count
    Write-Host "Team B pushed successfully" -ForegroundColor Green
    Write-Host "  Registry blobs after Team B: $blobsAfterB" -ForegroundColor Gray
    Write-Host "  Manifest layers: $($pushB.manifest.layers)" -ForegroundColor Gray

    # ============================================================
    # STEP 5: Verify deduplication
    # ============================================================
    Write-Host "`n========================================" -ForegroundColor Cyan
    Write-Host "STEP 5: Verify deduplication" -ForegroundColor Cyan
    Write-Host "========================================" -ForegroundColor Cyan

    # Team B should have the same number of layers as Team A
    if ($pushB.manifest.layers -ne $pushA.manifest.layers) {
        Write-Error "Layer count mismatch: Team A has $($pushA.manifest.layers), Team B has $($pushB.manifest.layers)"
    }
    Write-Host "Both teams have $($pushA.manifest.layers) layers" -ForegroundColor Green

    # The key test: blob count should NOT double.
    # If dedup works, blobsAfterB should be close to blobsAfterA + 1 (just the new TeamConfig)
    # In practice, workspace might also be shared, so it could be exactly +1.
    $blobIncrease = $blobsAfterB - $blobsAfterA
    $maxExpectedIncrease = 2  # TeamConfig + possibly workspace if different

    Write-Host "Blob increase after Team B push: $blobIncrease" -ForegroundColor Gray

    if ($blobIncrease -le $maxExpectedIncrease) {
        Write-Host "Deduplication confirmed: only $blobIncrease new blob(s) stored" -ForegroundColor Green
    } else {
        Write-Error "Deduplication failed: $blobIncrease new blobs stored (expected <= $maxExpectedIncrease)"
    }

    # Verify total blobs is less than sum of all unique layers
    $totalLayersIfNoDedup = $pushA.manifest.layers + $pushB.manifest.layers
    if ($blobsAfterB -lt $totalLayersIfNoDedup) {
        Write-Host "Cross-team deduplication verified: $blobsAfterB blobs < $totalLayersIfNoDedup total layers" -ForegroundColor Green
    } else {
        Write-Error "No cross-team deduplication: $blobsAfterB blobs >= $totalLayersIfNoDedup total layers"
    }

    # ============================================================
    # STEP 6: Pull Team B on a fresh local registry
    # ============================================================
    Write-Host "`n========================================" -ForegroundColor Cyan
    Write-Host "STEP 6: Pull Team B (fresh local registry)" -ForegroundColor Cyan
    Write-Host "========================================" -ForegroundColor Cyan

    $localRegistryDir = "$env:USERPROFILE/.peko/registry"
    if (Test-Path $localRegistryDir) {
        Remove-Item -Recurse -Force $localRegistryDir
        Write-Host "Cleared local registry store" -ForegroundColor Yellow
    }

    $importedTeamName = "team-beta-imported"
    $pullB = & $pekoCmd team pull $registryRefB --name $importedTeamName --json 2>&1 | ConvertFrom-Json
    if ($pullB.success -ne $true) {
        Write-Error "Team B pull failed: $($pullB | ConvertTo-Json)"
    }
    if ($pullB.name -ne $importedTeamName) {
        Write-Error "Team B pull returned wrong name: $($pullB.name)"
    }
    if ($pullB.agents_imported -ne 1) {
        Write-Error "Expected 1 agent imported, got $($pullB.agents_imported)"
    }
    Write-Host "Team B pulled and imported successfully" -ForegroundColor Green
    Write-Host "  Name: $($pullB.name)" -ForegroundColor Gray
    Write-Host "  Agents imported: $($pullB.agents_imported)" -ForegroundColor Gray

    # ============================================================
    # STEP 7: Verify imported team structure
    # ============================================================
    Write-Host "`n========================================" -ForegroundColor Cyan
    Write-Host "STEP 7: Verify imported team structure" -ForegroundColor Cyan
    Write-Host "========================================" -ForegroundColor Cyan

    $importedShow = & $pekoCmd team show $importedTeamName --json 2>&1 | ConvertFrom-Json
    if ($importedShow.agent_count -ne 1) {
        Write-Error "Imported team has wrong agent count: $($importedShow.agent_count)"
    }
    Write-Host "Imported team has $($importedShow.agent_count) agent(s)" -ForegroundColor Green

    # Verify agent exists
    $agentShow = & $pekoCmd agent show "$importedTeamName/$agentX" --json 2>&1 | ConvertFrom-Json
    if ($agentShow.name -ne $agentX) {
        Write-Error "Agent '$agentX' not found in imported team"
    }
    Write-Host "Agent '$agentX' verified in imported team" -ForegroundColor Green

    # Verify workspace files
    $importedWs = "$env:APPDATA/peko/workspaces/$importedTeamName/$agentX/NOTES.md"
    if (-not (Test-Path $importedWs)) {
        Write-Error "Missing workspace file: $importedWs"
    }
    $wsContent = Get-Content $importedWs -Raw
    if ($wsContent -notmatch "Shared workspace content") {
        Write-Error "Workspace content mismatch"
    }
    Write-Host "Workspace files verified" -ForegroundColor Green

    # ============================================================
    # STEP 8: Pull Team A and verify both teams coexist
    # ============================================================
    Write-Host "`n========================================" -ForegroundColor Cyan
    Write-Host "STEP 8: Pull Team A (verify coexistence)" -ForegroundColor Cyan
    Write-Host "========================================" -ForegroundColor Cyan

    $importedTeamAName = "team-alpha-imported"
    $pullA = & $pekoCmd team pull $registryRefA --name $importedTeamAName --json 2>&1 | ConvertFrom-Json
    if ($pullA.success -ne $true) {
        Write-Error "Team A pull failed: $($pullA | ConvertTo-Json)"
    }
    if ($pullA.agents_imported -ne 1) {
        Write-Error "Expected 1 agent imported from Team A, got $($pullA.agents_imported)"
    }
    Write-Host "Team A pulled and imported successfully" -ForegroundColor Green

    # Verify both teams have the same agent functioning
    $agentAShow = & $pekoCmd agent show "$importedTeamAName/$agentX" --json 2>&1 | ConvertFrom-Json
    $agentBShow = & $pekoCmd agent show "$importedTeamName/$agentX" --json 2>&1 | ConvertFrom-Json
    if ($agentAShow.name -ne $agentX -or $agentBShow.name -ne $agentX) {
        Write-Error "Agent mismatch between imported teams"
    }
    Write-Host "Both imported teams have functioning Agent X" -ForegroundColor Green

    # Verify workspace in Team A import
    $importedWsA = "$env:APPDATA/peko/workspaces/$importedTeamAName/$agentX/NOTES.md"
    if (-not (Test-Path $importedWsA)) {
        Write-Error "Missing workspace file in Team A import: $importedWsA"
    }
    Write-Host "Team A workspace verified" -ForegroundColor Green

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

    try { & $pekoCmd team remove $teamA --force 2>&1 | Out-Null } catch {}
    try { & $pekoCmd team remove $teamB --force 2>&1 | Out-Null } catch {}
    try { & $pekoCmd team remove $importedTeamName --force 2>&1 | Out-Null } catch {}
    try { & $pekoCmd team remove $importedTeamAName --force 2>&1 | Out-Null } catch {}
    Write-Host "Removed test teams" -ForegroundColor Green
}

if ($failed) {
    exit 1
}

Write-Host "`n✅ All team registry deduplication tests completed!" -ForegroundColor Green
