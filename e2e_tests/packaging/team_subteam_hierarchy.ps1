#!/usr/bin/env pwsh
# Team / Subteam Hierarchy E2E Test
#
# Real-world scenario:
#   1. Create a parent team with multiple subteams (simulated via team naming or nesting).
#   2. Assign agents to different subteams with different roles.
#   3. Export the entire hierarchy as a .team snapshot.
#   4. Push to registry, pull on a fresh machine, and import.
#   5. Verify the hierarchy, agent roles, and workspace isolation are preserved.
#
# NOTE: True subteam nesting may not be fully implemented yet. This test uses
# multiple teams to simulate a hierarchy and exports them individually, then
# verifies cross-team agent references and workspace isolation.
#
# Deterministic verification:
#   - Structural checks: agent counts per team, workspace paths, config values.

param(
    [string]$Provider = "minimax",
    [int]$RegistryPort = 18771
)

$ErrorActionPreference = "Stop"

Write-Host "========================================" -ForegroundColor Cyan
Write-Host "Team / Subteam Hierarchy E2E Test" -ForegroundColor Cyan
Write-Host "========================================" -ForegroundColor Cyan

# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------

function Start-MockRegistry {
    param([int]$Port)
    $outLog = "$env:TEMP\PEKO_mock_registry_out_$Port.log"
    $errLog = "$env:TEMP\PEKO_mock_registry_err_$Port.log"
    if (Test-Path $outLog) { Remove-Item $outLog }
    if (Test-Path $errLog) { Remove-Item $errLog }

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

$testDir = "$env:TEMP/PEKO_hierarchy_test_$([System.Guid]::NewGuid().ToString().Substring(0,8))"
New-Item -ItemType Directory -Path $testDir -Force | Out-Null
Write-Host "Test directory: $testDir" -ForegroundColor Gray

$failed = $false

try {
    # ============================================================
    # STEP 1: Create parent team and subteams
    # ============================================================
    Write-Host "`n========================================" -ForegroundColor Cyan
    Write-Host "STEP 1: Create parent team and subteams" -ForegroundColor Cyan
    Write-Host "========================================" -ForegroundColor Cyan

    $parentTeam = "engineering"
    $subTeam1 = "engineering-frontend"
    $subTeam2 = "engineering-backend"
    $subTeam3 = "engineering-ops"

    & $pekoCmd team create $parentTeam --description "Parent engineering team" 2>&1 | Out-Null
    & $pekoCmd team create $subTeam1 --description "Frontend subteam" 2>&1 | Out-Null
    & $pekoCmd team create $subTeam2 --description "Backend subteam" 2>&1 | Out-Null
    & $pekoCmd team create $subTeam3 --description "Ops subteam" 2>&1 | Out-Null
    Write-Host "Created parent team and 3 subteams" -ForegroundColor Green

    # ============================================================
    # STEP 2: Create agents in each subteam with roles
    # ============================================================
    Write-Host "`n========================================" -ForegroundColor Cyan
    Write-Host "STEP 2: Create agents in subteams" -ForegroundColor Cyan
    Write-Host "========================================" -ForegroundColor Cyan

    $agentsFrontend = @("ui-designer", "react-dev")
    $agentsBackend = @("api-dev", "db-admin")
    $agentsOps = @("sre", "ci-cd")

    foreach ($agent in $agentsFrontend) {
        & $pekoCmd agent create "$subTeam1/$agent" --provider $Provider 2>&1 | Out-Null
    }
    foreach ($agent in $agentsBackend) {
        & $pekoCmd agent create "$subTeam2/$agent" --provider $Provider 2>&1 | Out-Null
    }
    foreach ($agent in $agentsOps) {
        & $pekoCmd agent create "$subTeam3/$agent" --provider $Provider 2>&1 | Out-Null
    }
    Write-Host "Created agents in all subteams" -ForegroundColor Green

    # Add workspace content per agent
    foreach ($agent in $agentsFrontend) {
        $ws = "$env:APPDATA/peko/workspaces/$subTeam1/$agent"
        New-Item -ItemType Directory -Path $ws -Force | Out-Null
        "# Role`nFrontend developer specializing in React." | Out-File -FilePath "$ws/ROLE.md" -Encoding UTF8
    }
    foreach ($agent in $agentsBackend) {
        $ws = "$env:APPDATA/peko/workspaces/$subTeam2/$agent"
        New-Item -ItemType Directory -Path $ws -Force | Out-Null
        "# Role`nBackend developer specializing in Rust APIs." | Out-File -FilePath "$ws/ROLE.md" -Encoding UTF8
    }
    foreach ($agent in $agentsOps) {
        $ws = "$env:APPDATA/peko/workspaces/$subTeam3/$agent"
        New-Item -ItemType Directory -Path $ws -Force | Out-Null
        "# Role`nSite reliability engineer." | Out-File -FilePath "$ws/ROLE.md" -Encoding UTF8
    }
    Write-Host "Added workspace content to all agents" -ForegroundColor Green

    # ============================================================
    # STEP 3: Export each subteam
    # ============================================================
    Write-Host "`n========================================" -ForegroundColor Cyan
    Write-Host "STEP 3: Export each subteam" -ForegroundColor Cyan
    Write-Host "========================================" -ForegroundColor Cyan

    $export1 = "$testDir/frontend.team"
    $export2 = "$testDir/backend.team"
    $export3 = "$testDir/ops.team"

    & $pekoCmd team export $subTeam1 -o $export1 --json 2>&1 | Out-Null
    & $pekoCmd team export $subTeam2 -o $export2 --json 2>&1 | Out-Null
    & $pekoCmd team export $subTeam3 -o $export3 --json 2>&1 | Out-Null

    if (-not (Test-Path $export1)) { Write-Error "Frontend export failed" }
    if (-not (Test-Path $export2)) { Write-Error "Backend export failed" }
    if (-not (Test-Path $export3)) { Write-Error "Ops export failed" }
    Write-Host "Exported all 3 subteams" -ForegroundColor Green

    # ============================================================
    # STEP 4: Push subteam snapshots to registry
    # ============================================================
    Write-Host "`n========================================" -ForegroundColor Cyan
    Write-Host "STEP 4: Push subteam snapshots to registry" -ForegroundColor Cyan
    Write-Host "========================================" -ForegroundColor Cyan

    & $pekoCmd team push $subTeam1 "127.0.0.1:$RegistryPort/peko/teams/frontend:latest" 2>&1 | Out-Null
    & $pekoCmd team push $subTeam2 "127.0.0.1:$RegistryPort/peko/teams/backend:latest" 2>&1 | Out-Null
    & $pekoCmd team push $subTeam3 "127.0.0.1:$RegistryPort/peko/teams/ops:latest" 2>&1 | Out-Null
    Write-Host "Pushed all subteams to registry" -ForegroundColor Green

    # ============================================================
    # STEP 5: Simulate fresh machine — remove originals and pull/import
    # ============================================================
    Write-Host "`n========================================" -ForegroundColor Cyan
    Write-Host "STEP 5: Fresh machine pull" -ForegroundColor Cyan
    Write-Host "========================================" -ForegroundColor Cyan

    $imported1 = "frontend-clone"
    $imported2 = "backend-clone"
    $imported3 = "ops-clone"

    # Remove original teams
    & $pekoCmd team remove $subTeam1 --force 2>&1 | Out-Null
    & $pekoCmd team remove $subTeam2 --force 2>&1 | Out-Null
    & $pekoCmd team remove $subTeam3 --force 2>&1 | Out-Null
    Write-Host "Removed original subteams" -ForegroundColor Yellow

    # team pull auto-imports, so we use --name to set the imported team name
    & $pekoCmd team pull "127.0.0.1:$RegistryPort/peko/teams/frontend:latest" --name $imported1 2>&1 | Out-Null
    & $pekoCmd team pull "127.0.0.1:$RegistryPort/peko/teams/backend:latest" --name $imported2 2>&1 | Out-Null
    & $pekoCmd team pull "127.0.0.1:$RegistryPort/peko/teams/ops:latest" --name $imported3 2>&1 | Out-Null
    Write-Host "Pulled and imported all subteams from registry" -ForegroundColor Green

    # ============================================================
    # STEP 7: Verify agent counts and workspace isolation
    # ============================================================
    Write-Host "`n========================================" -ForegroundColor Cyan
    Write-Host "STEP 7: Verify hierarchy integrity" -ForegroundColor Cyan
    Write-Host "========================================" -ForegroundColor Cyan

    $show1 = & $pekoCmd team show $imported1 --json 2>&1 | ConvertFrom-Json
    $show2 = & $pekoCmd team show $imported2 --json 2>&1 | ConvertFrom-Json
    $show3 = & $pekoCmd team show $imported3 --json 2>&1 | ConvertFrom-Json

    if ($show1.agent_count -ne 2) { Write-Error "Frontend clone has wrong agent count: $($show1.agent_count)" }
    if ($show2.agent_count -ne 2) { Write-Error "Backend clone has wrong agent count: $($show2.agent_count)" }
    if ($show3.agent_count -ne 2) { Write-Error "Ops clone has wrong agent count: $($show3.agent_count)" }
    Write-Host "Agent counts verified (2 each)" -ForegroundColor Green

    # Verify workspace files and isolation
    $checks = @(
        @($imported1, "ui-designer", "React"),
        @($imported1, "react-dev", "React"),
        @($imported2, "api-dev", "Rust"),
        @($imported2, "db-admin", "Rust"),
        @($imported3, "sre", "reliability"),
        @($imported3, "ci-cd", "reliability")
    )

    foreach ($check in $checks) {
        $team = $check[0]
        $agent = $check[1]
        $expectedContent = $check[2]
        $roleFile = "$env:APPDATA/peko/workspaces/$team/$agent/ROLE.md"
        if (-not (Test-Path $roleFile)) { Write-Error "Missing workspace file: $roleFile" }
        $content = Get-Content $roleFile -Raw
        if ($content -notmatch $expectedContent) { Write-Error "Workspace content mismatch for $team/$agent" }
    }
    Write-Host "Workspace isolation and content verified" -ForegroundColor Green

    # ============================================================
    # STEP 8: Verify cross-team agent references work
    # ============================================================
    Write-Host "`n========================================" -ForegroundColor Cyan
    Write-Host "STEP 8: Cross-team agent show" -ForegroundColor Cyan
    Write-Host "========================================" -ForegroundColor Cyan

    foreach ($agent in $agentsFrontend) {
        $show = & $pekoCmd agent show "$imported1/$agent" --json 2>&1 | ConvertFrom-Json
        if ($show.name -ne $agent) { Write-Error "Agent $agent not found in $imported1" }
    }
    Write-Host "Cross-team agent references work" -ForegroundColor Green

    # ============================================================
    # STEP 9: Re-import with --force (idempotency)
    # ============================================================
    Write-Host "`n========================================" -ForegroundColor Cyan
    Write-Host "STEP 9: Re-import with --force" -ForegroundColor Cyan
    Write-Host "========================================" -ForegroundColor Cyan

    $reimport = & $pekoCmd team import $export1 --name $imported1 --force --json 2>&1 | ConvertFrom-Json
    if ($reimport.name -ne $imported1) { Write-Error "Re-import with --force failed" }
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

    try { & $pekoCmd team remove $parentTeam --force 2>&1 | Out-Null } catch {}
    try { & $pekoCmd team remove $imported1 --force 2>&1 | Out-Null } catch {}
    try { & $pekoCmd team remove $imported2 --force 2>&1 | Out-Null } catch {}
    try { & $pekoCmd team remove $imported3 --force 2>&1 | Out-Null } catch {}
    Write-Host "Removed test teams" -ForegroundColor Green
}

if ($failed) {
    exit 1
}

Write-Host "`n✅ All team hierarchy tests completed!" -ForegroundColor Green
